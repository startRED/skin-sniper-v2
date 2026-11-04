//! Descoberta e seleção do que vigiar dado o saldo (docs/projeto.md, "Caça" 1–2).
//!
//! Um ciclo, um passo por vez (cada passo é uma consulta à Steam, no ritmo do limitador):
//! 1. varredura de `search/render` por popularidade, uma página por categoria por rodada (cache de 24 h);
//! 2. razão BRL/USD medida entre um anúncio em BRL e o preço USD do mesmo item (nunca fixa no código);
//! 3. `QueryPriceHistory` dos candidatos com piso ≤ saldo (cache de 6 h);
//! 4. seleção por nota = vendas/dia × dispersão × (1 + acertos), mais as consultas de grupo raro.
//!
//! shortcut: a varredura cobre `SCAN_ROUNDS` páginas (60 itens) por categoria, não o catálogo inteiro (cada
//! página custa uma consulta e há milhares); subir se faltarem candidatos para o saldo.

use super::scheduler::Task;
use super::{Hunt, detect};
use crate::db::Db;
use crate::pricing::groups::seed_ranges;
use crate::pricing::sell::{PricePoint, weighted_quantile};
use crate::steam::client::Error as SteamError;
use crate::steam::listings::{self, paint_index_from_code};
use crate::steam::search;
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::collections::{HashMap, HashSet, VecDeque};

pub const SCAN_ROUNDS: u32 = 6;
pub const HISTORY_TTL: i64 = 6 * 3600;
pub const CATALOG_TTL: i64 = 24 * 3600;
pub const MIN_SALES_PER_DAY: f64 = 3.0;
pub const WATCH_MAX: usize = 40;
pub const RARE_MAX_TASKS: usize = 24;
const RATIO_SAMPLES: usize = 3;
const RATIO_MAX_TRIES: u32 = 6;
/// Itens de US$ 1 a 20 servem para medir a razão: acima de US$ 1 o arredondamento do centavo some.
const RATIO_USD_RANGE: (i64, i64) = (100, 2000);
const HISTORY_PER_ROUND: usize = 25;
/// Reseleciona a cada tantos históricos novos, para a vigilância começar antes do fim da descoberta.
const RESELECT_EVERY: u32 = 5;
const RATIO_KEY: &str = "usd_brl_ratio";
const SCAN_KEY: &str = "catalog_scan_at";
const DAY: i64 = 86_400;

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub name: String,
    pub code: Option<String>,
    pub floor_usd_cents: Option<i64>,
    pub rank: i64,
    pub sales_per_day: Option<f64>,
    pub dispersion: Option<f64>,
    pub history_at: Option<i64>,
}

// ---------------------------------------------------------------------------------------------
// Funções puras

/// Piso em USD → BRL pela razão medida. Só serve para filtrar candidatos; o preço final vem do anúncio.
pub fn usd_to_brl(usd_cents: i64, ratio: f64) -> i64 {
    (usd_cents as f64 * ratio).round() as i64
}

pub fn median(v: &[f64]) -> Option<f64> {
    let mut s: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if s.is_empty() {
        return None;
    }
    s.sort_by(f64::total_cmp);
    let m = s.len() / 2;
    Some(if s.len() % 2 == 1 {
        s[m]
    } else {
        (s[m - 1] + s[m]) / 2.0
    })
}

/// Desgaste do `market_hash_name` como o filtro `Exterior` da Valve: FN=0 … BS=4.
pub fn wear_of(name: &str) -> Option<u8> {
    match wear_label(name)? {
        "Factory New" => Some(0),
        "Minimal Wear" => Some(1),
        "Field-Tested" => Some(2),
        "Well-Worn" => Some(3),
        "Battle-Scarred" => Some(4),
        _ => None,
    }
}

/// "Glock-18 | Moonrise (Field-Tested)" → "Field-Tested".
pub fn wear_label(name: &str) -> Option<&str> {
    name.rsplit_once(" (")?.1.strip_suffix(')')
}

/// Nome sem o desgaste.
pub fn base_name(name: &str) -> &str {
    name.rsplit_once(" (").map_or(name, |(b, _)| b)
}

/// Vendas/dia e dispersão relativa (P50−P10)/P50 dos últimos 7 dias, ponderadas por compras.
pub fn window_stats(points: &[PricePoint], now: i64) -> Option<(f64, f64)> {
    let pairs: Vec<(f64, f64)> = points
        .iter()
        .filter(|p| p.ts > now - 7 * DAY && p.ts <= now && p.purchases > 0)
        .map(|p| (p.median_cents as f64, f64::from(p.purchases)))
        .collect();
    let total: f64 = pairs.iter().map(|p| p.1).sum();
    if total <= 0.0 {
        return None;
    }
    let p50 = weighted_quantile(&pairs, 0.5)?;
    let p10 = weighted_quantile(&pairs, 0.1)?;
    Some((total / 7.0, ((p50 - p10) / p50).max(0.0)))
}

/// Nota de um item. Dispersão tem piso de 2 % para um item estável não zerar a nota.
pub fn score(sales_per_day: f64, dispersion: f64, hits: u32) -> f64 {
    sales_per_day * dispersion.max(0.02) * (1.0 + f64::from(hits))
}

/// Mudou mais de 20 % (ou não havia saldo antes).
pub fn saldo_moved(old: i64, new: i64) -> bool {
    old <= 0 || (new - old).abs() * 100 > old * 20
}

/// Itens com piso ≤ saldo e ≥ 3 vendas/dia por nota (até `WATCH_MAX`), mais as consultas de grupo raro das
/// skins com tabela e piso ≤ saldo (até `RARE_MAX_TASKS`).
pub fn select(items: &[Item], ratio: f64, saldo: i64, hits: &HashMap<String, u32>) -> Vec<Task> {
    let floor = |i: &Item| i.floor_usd_cents.map(|u| usd_to_brl(u, ratio));
    let mut common: Vec<(f64, Task)> = items
        .iter()
        .filter_map(|i| {
            let (code, f) = (i.code.clone()?, floor(i)?);
            let spd = i.sales_per_day.filter(|s| *s >= MIN_SALES_PER_DAY)?;
            if f > saldo {
                return None;
            }
            let id = format!("d:{}", i.name);
            let base = score(spd, i.dispersion.unwrap_or(0.0), 0);
            let with_hits = base * (1.0 + f64::from(hits.get(&id).copied().unwrap_or(0)));
            Some((
                with_hits,
                Task {
                    id,
                    name: i.name.clone(),
                    code,
                    wear: wear_of(&i.name),
                    seeds: None,
                    score: base,
                },
            ))
        })
        .collect();
    common.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    common.truncate(WATCH_MAX);
    let mean = if common.is_empty() {
        10.0
    } else {
        common.iter().map(|c| c.1.score).sum::<f64>() / common.len() as f64
    };
    let mut tasks: Vec<Task> = common.into_iter().map(|c| c.1).collect();

    // Raros: um código por skin (cobre todos os desgastes), pelo menor piso entre os desgastes conhecidos.
    let mut by_code: HashMap<&str, (i64, &str)> = HashMap::new();
    for i in items {
        if let (Some(code), Some(f)) = (i.code.as_deref(), floor(i)) {
            let e = by_code.entry(code).or_insert((f, i.name.as_str()));
            if f < e.0 {
                *e = (f, i.name.as_str());
            }
        }
    }
    let mut rare: Vec<(&str, i64, &str)> = by_code
        .into_iter()
        .filter(|(_, (f, name))| *f <= saldo && !seed_ranges(name).is_empty())
        .map(|(code, (f, name))| (code, f, name))
        .collect();
    rare.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(b.0)));
    'outer: for (code, _, name) in rare {
        for (lo, hi) in seed_ranges(name) {
            if tasks.iter().filter(|t| t.seeds.is_some()).count() >= RARE_MAX_TASKS {
                break 'outer;
            }
            tasks.push(Task {
                id: format!("r:{code}:{lo}-{hi}"),
                name: base_name(name).to_string(),
                code: code.to_string(),
                wear: None,
                seeds: Some((lo, hi)),
                // Anúncio raro aparece pouco: metade do peso de uma tarefa média.
                score: mean * 0.5,
            });
        }
    }
    tasks
}

// ---------------------------------------------------------------------------------------------
// Máquina de passos

#[derive(Default)]
pub struct Discovery {
    started: Option<i64>,
    finished: bool,
    planned_saldo: i64,
    /// Há varredura neste ciclo (catálogo com mais de 24 h ou sem catálogo).
    scanning: bool,
    round: u32,
    scan_left: VecDeque<search::Category>,
    history: VecDeque<String>,
    hist_ready: bool,
    want_samples: bool,
    samples: Vec<f64>,
    tries: u32,
    tried: HashSet<String>,
    done_hist: u32,
    /// Tamanho do último lote de candidatos: lote cheio indica que pode haver mais.
    last_batch: usize,
    /// Históricos já tentados neste ciclo (um que falha sempre não volta a ser candidato).
    hist_tried: HashSet<String>,
}

impl Discovery {
    fn begin(&mut self, now: i64, saldo: i64, scanning: bool, want_samples: bool) {
        *self = Discovery {
            started: Some(now),
            planned_saldo: saldo,
            scanning,
            scan_left: if scanning {
                search::Category::ALL.into_iter().collect()
            } else {
                VecDeque::new()
            },
            want_samples,
            ..Discovery::default()
        };
    }

    /// Hora de começar outro ciclo: 24 h, saldo mudou > 20 %, ou ficou sem câmbio e já passaram 5 min.
    fn due(&self, now: i64, saldo: i64, ratio_known: bool) -> bool {
        match self.started {
            None => true,
            Some(t) => {
                self.finished
                    && (now - t >= CATALOG_TTL
                        || saldo_moved(self.planned_saldo, saldo)
                        || (!ratio_known && now - t >= 300))
            }
        }
    }
}

enum Next {
    Scan(search::Category, u32),
    Ratio(String, String, Option<u8>, i64),
    Candidates,
    History(String),
    Nothing,
}

/// Executa o próximo passo da descoberta. `Ok(true)` = gastou uma consulta (ou trabalho de banco que
/// destrava a próxima); `Ok(false)` = nada a fazer agora. Erros de rede sobem para o laço recuar.
pub async fn step(h: &Hunt) -> Result<bool> {
    let now = h.now();
    let (due, saldo) = {
        let s = h.lock();
        (s.disc.due(now, s.saldo, s.ratio.is_some()), s.saldo)
    };
    if due {
        let scan_at = get_setting(&h.db, SCAN_KEY)
            .await?
            .and_then(|v| v.parse::<i64>().ok());
        let mut s = h.lock();
        let scanning = scan_at.is_none_or(|t| now - t >= CATALOG_TTL);
        let want = s.ratio.is_none_or(|(_, t)| now - t >= CATALOG_TTL);
        s.disc.begin(now, saldo, scanning, want);
    }
    loop {
        match next_step(h, now).await? {
            Next::Nothing => return Ok(false),
            // só banco, sem consulta à Steam: segue para o passo seguinte sem gastar a vaga
            Next::Candidates => {
                let exclude = h.lock().disc.hist_tried.clone();
                let list = history_candidates(h, now, saldo, &exclude).await?;
                let mut s = h.lock();
                s.disc.last_batch = list.len();
                s.disc.history = list.into();
                s.disc.hist_ready = true;
            }
            Next::Scan(cat, start) => {
                let r = scan_page(h, cat, start).await;
                finish_scan(h, cat, r).await?;
                return Ok(true);
            }
            Next::Ratio(name, code, wear, usd) => {
                let r = measure_ratio_sample(h, &name, &code, wear, usd).await;
                if r.as_ref().err().is_some_and(retryable) {
                    h.lock().disc.tried.remove(&name); // não conta: a vaga nem chegou à Steam
                } else {
                    h.lock().disc.tries += 1;
                }
                r?;
                return Ok(true);
            }
            Next::History(name) => {
                let r = detect::fetch_and_store_history(h, &name).await;
                {
                    let mut s = h.lock();
                    if r.as_ref().err().is_some_and(retryable) {
                        s.disc.history.push_front(name);
                    } else {
                        s.disc.hist_tried.insert(name);
                        s.disc.done_hist += 1;
                    }
                }
                r?;
                let reselect = {
                    let s = h.lock();
                    s.disc.done_hist >= RESELECT_EVERY || s.disc.history.is_empty()
                };
                if reselect {
                    h.lock().disc.done_hist = 0;
                    reselect_now(h).await?;
                }
                return Ok(true);
            }
        }
    }
}

fn retryable(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<SteamError>(),
        Some(SteamError::RateLimited { .. } | SteamError::BreakerOpen { .. })
    )
}

async fn next_step(h: &Hunt, now: i64) -> Result<Next> {
    // Decide sob o lock o que dá para decidir sem banco; o resto precisa de consulta ao banco.
    enum Pre {
        Done(Next),
        NeedSample,
        Advance,
        Again,
    }
    let pre = {
        let mut s = h.lock();
        let ratio_known = s.ratio.is_some();
        let d = &mut s.disc;
        if let Some(cat) = d.scan_left.front().copied() {
            d.scan_left.pop_front();
            Pre::Done(Next::Scan(cat, d.round * 10))
        } else if d.want_samples && d.samples.len() < RATIO_SAMPLES && d.tries < RATIO_MAX_TRIES {
            Pre::NeedSample
        } else if !d.hist_ready {
            Pre::Done(Next::Candidates)
        } else if let Some(name) = d.history.pop_front() {
            Pre::Done(Next::History(name))
        } else if d.last_batch >= HISTORY_PER_ROUND {
            d.hist_ready = false; // lote cheio: pode haver mais candidatos nesta rodada
            d.last_batch = 0;
            Pre::Again
        } else if d.scanning && d.round + 1 < SCAN_ROUNDS {
            Pre::Advance
        } else {
            d.finished = true;
            if !ratio_known {
                s.message = Some("sem câmbio medido: aguardando uma amostra da Steam".into());
            }
            Pre::Done(Next::Nothing)
        }
    };
    match pre {
        Pre::Done(n) => Ok(n),
        Pre::Again => Box::pin(next_step(h, now)).await,
        Pre::Advance => {
            {
                let mut s = h.lock();
                s.disc.round += 1;
                s.disc.scan_left = search::Category::ALL.into_iter().collect();
                s.disc.hist_ready = false;
            }
            Box::pin(next_step(h, now)).await
        }
        Pre::NeedSample => {
            let tried = h.lock().disc.tried.clone();
            let items = load_items(&h.db).await?;
            let cand = items.iter().find(|i| {
                i.code.is_some()
                    && wear_of(&i.name).is_some()
                    && !tried.contains(&i.name)
                    && i.floor_usd_cents
                        .is_some_and(|u| (RATIO_USD_RANGE.0..=RATIO_USD_RANGE.1).contains(&u))
            });
            let picked = {
                let mut s = h.lock();
                match cand {
                    Some(i) => {
                        s.disc.tried.insert(i.name.clone());
                        Some(Next::Ratio(
                            i.name.clone(),
                            i.code.clone().unwrap_or_default(),
                            wear_of(&i.name),
                            i.floor_usd_cents.unwrap_or_default(),
                        ))
                    }
                    None => {
                        // Sem item para medir: segue sem amostras (candidatos esperam a razão).
                        s.disc.want_samples = false;
                        None
                    }
                }
            };
            match picked {
                Some(n) => Ok(n),
                None => Box::pin(next_step(h, now)).await,
            }
        }
    }
}

async fn scan_page(h: &Hunt, cat: search::Category, start: u32) -> Result<()> {
    let page = search::fetch_page(&h.steam, cat, start).await?;
    let now = h.now();
    let ratio = h.lock().ratio.map(|r| r.0);
    let entries = page.entries;
    h.db
        .call(move |c| {
            let tx = c.transaction()?;
            for (i, e) in entries.iter().enumerate() {
                let paint = e
                    .item_code
                    .as_deref()
                    .and_then(paint_index_from_code)
                    .map(i64::from);
                let floor_brl = ratio.map(|r| usd_to_brl(e.sell_price_usd_cents, r));
                tx.execute(
                    "INSERT INTO items(name, g_code, paint_index, floor_cents, floor_usd_cents, icon_url, rank, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(name) DO UPDATE SET g_code = excluded.g_code, paint_index = excluded.paint_index,
                        floor_cents = excluded.floor_cents, floor_usd_cents = excluded.floor_usd_cents,
                        icon_url = excluded.icon_url, rank = excluded.rank, updated_at = excluded.updated_at",
                    params![
                        e.name,
                        e.item_code,
                        paint,
                        floor_brl,
                        e.sell_price_usd_cents,
                        e.icon_url,
                        i64::from(start) + i as i64,
                        now
                    ],
                )?;
            }
            tx.commit()
        })
        .await?;
    Ok(())
}

/// Depois de uma página: erro não retentável não trava a rodada; fim de rodada marca a varredura como feita.
async fn finish_scan(h: &Hunt, cat: search::Category, r: Result<()>) -> Result<()> {
    if let Err(e) = r {
        if retryable(&e) {
            h.lock().disc.scan_left.push_front(cat); // a página volta à frente da fila
        }
        return Err(e);
    }
    let round_done = h.lock().disc.scan_left.is_empty();
    if round_done {
        set_setting(&h.db, SCAN_KEY, h.now().to_string()).await?;
    }
    Ok(())
}

async fn measure_ratio_sample(
    h: &Hunt,
    name: &str,
    code: &str,
    wear: Option<u8>,
    usd_cents: i64,
) -> Result<()> {
    let page = listings::query_listings_wear(&h.steam, code, 0, wear, &[]).await?;
    let Some(brl) = page
        .listings
        .iter()
        .filter(|l| l.name == name)
        .map(|l| l.price_cents)
        .min()
    else {
        return Ok(());
    };
    let now = h.now();
    let ratio = {
        let mut s = h.lock();
        s.disc.samples.push(brl as f64 / usd_cents as f64);
        let r = median(&s.disc.samples).unwrap_or(0.0);
        s.ratio = Some((r, now));
        r
    };
    save_ratio(&h.db, ratio, now).await?;
    tracing::info!(
        ratio = format!("{ratio:.4}"),
        amostras = h.lock().disc.samples.len(),
        "razão BRL/USD medida"
    );
    Ok(())
}

async fn history_candidates(
    h: &Hunt,
    now: i64,
    saldo: i64,
    exclude: &HashSet<String>,
) -> Result<Vec<String>> {
    let Some((ratio, _)) = h.lock().ratio else {
        return Ok(vec![]);
    };
    let items = load_items(&h.db).await?;
    Ok(items
        .into_iter()
        .filter(|i| {
            i.code.is_some()
                && i.floor_usd_cents
                    .is_some_and(|u| usd_to_brl(u, ratio) <= saldo)
                && i.history_at.is_none_or(|t| now - t >= HISTORY_TTL)
                && !exclude.contains(&i.name)
        })
        .take(HISTORY_PER_ROUND)
        .map(|i| i.name)
        .collect())
}

// ---------------------------------------------------------------------------------------------
// Seleção

/// Reseleciona se o saldo mudou mais de 20 % ou passaram 24 h desde a última seleção.
pub async fn maybe_reselect(h: &Hunt) -> Result<()> {
    let now = h.now();
    let due = {
        let s = h.lock();
        s.ratio.is_some()
            && s.selected
                .is_none_or(|(sal, t)| saldo_moved(sal, s.saldo) || now - t >= CATALOG_TTL)
    };
    if due { reselect_now(h).await } else { Ok(()) }
}

pub async fn reselect_now(h: &Hunt) -> Result<()> {
    let now = h.now();
    let (saldo, ratio) = {
        let s = h.lock();
        (s.saldo, s.ratio)
    };
    let Some((ratio, _)) = ratio else {
        return Ok(());
    };
    let items = load_items(&h.db).await?;
    let hits = recent_hits(&h.db, now).await?;
    let tasks = select(&items, ratio, saldo, &hits);
    let (n, removed) = {
        let mut s = h.lock();
        let removed = s.queue.set_tasks(tasks, &hits, now * 1000);
        s.selected = Some((saldo, now));
        (s.queue.len(), removed)
    };
    tracing::info!(
        tarefas = n,
        sairam = removed.len(),
        saldo,
        "seleção atualizada"
    );
    if !removed.is_empty() {
        detect::drop_tasks(h, &removed).await?;
    }
    h.publish_state();
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Banco

pub async fn load_items(db: &Db) -> Result<Vec<Item>> {
    db.call(|c| {
        c.prepare(
            "SELECT name, g_code, floor_usd_cents, COALESCE(rank, 1000000), sales_per_day, dispersion, history_at
             FROM items ORDER BY COALESCE(rank, 1000000), name",
        )?
        .query_map([], |r| {
            Ok(Item {
                name: r.get(0)?,
                code: r.get(1)?,
                floor_usd_cents: r.get(2)?,
                rank: r.get(3)?,
                sales_per_day: r.get(4)?,
                dispersion: r.get(5)?,
                history_at: r.get(6)?,
            })
        })?
        .collect()
    })
    .await
}

/// Acertos das últimas 24 h por tarefa.
async fn recent_hits(db: &Db, now: i64) -> Result<HashMap<String, u32>> {
    let rows: Vec<(String, u32)> = db
        .call(move |c| {
            c.prepare(
                "SELECT task, count(*) FROM opportunities WHERE task IS NOT NULL AND created_at >= ?1 GROUP BY task",
            )?
            .query_map([now - DAY], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
        })
        .await?;
    Ok(rows.into_iter().collect())
}

async fn get_setting(db: &Db, key: &'static str) -> Result<Option<String>> {
    db.call(move |c| {
        c.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()
    })
    .await
}

async fn set_setting(db: &Db, key: &'static str, value: String) -> Result<()> {
    db.call(move |c| {
        c.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map(|_| ())
    })
    .await
}

async fn save_ratio(db: &Db, ratio: f64, at: i64) -> Result<()> {
    set_setting(db, RATIO_KEY, format!("{ratio}|{at}")).await
}

pub async fn load_ratio(db: &Db) -> Result<Option<(f64, i64)>> {
    Ok(get_setting(db, RATIO_KEY).await?.and_then(|v| {
        let (r, t) = v.split_once('|')?;
        Some((
            r.parse().ok().filter(|r: &f64| r.is_finite() && *r > 0.0)?,
            t.parse().ok()?,
        ))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steam::history;

    fn item(name: &str, usd: i64, rank: i64, spd: f64, disp: f64) -> Item {
        Item {
            name: name.into(),
            code: Some(format!("G{:08X}1", name.len() * 7 + rank as usize)),
            floor_usd_cents: Some(usd),
            rank,
            sales_per_day: Some(spd),
            dispersion: Some(disp),
            history_at: Some(0),
        }
    }

    #[test]
    fn wear_parsing() {
        assert_eq!(wear_of("Glock-18 | Moonrise (Field-Tested)"), Some(2));
        assert_eq!(wear_of("AK-47 | Redline (Battle-Scarred)"), Some(4));
        assert_eq!(wear_of("Sealed Terminal"), None);
        assert_eq!(
            base_name("Glock-18 | Moonrise (Field-Tested)"),
            "Glock-18 | Moonrise"
        );
        assert_eq!(wear_label("X (Weird)"), Some("Weird"));
        assert_eq!(wear_of("X (Weird)"), None);
    }

    #[test]
    fn usd_to_brl_uses_the_measured_ratio() {
        assert_eq!(usd_to_brl(461, 5.5), 2536); // 25,36 reais
        assert_eq!(usd_to_brl(100, 5.0), 500);
        assert_eq!(median(&[5.4, 5.6, 5.5]), Some(5.5));
        assert_eq!(median(&[5.0, 6.0]), Some(5.5));
        assert_eq!(median(&[]), None);
    }

    #[test]
    fn saldo_change_threshold_is_twenty_percent() {
        assert!(!saldo_moved(3608, 3608));
        assert!(!saldo_moved(3608, 4300)); // +19,2 %
        assert!(saldo_moved(3608, 4400)); // +21,9 %
        assert!(saldo_moved(3608, 2800)); // −22,4 %
        assert!(saldo_moved(0, 100));
    }

    #[test]
    fn stats_from_the_real_history_capture() {
        let s = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/steam_pricehistory_moonrise_ft.json"
        ))
        .unwrap();
        let data = serde_json::from_str::<serde_json::Value>(&s).unwrap()["data"].take();
        let pts: Vec<PricePoint> = history::parse_history(&data)
            .unwrap()
            .into_iter()
            .map(|p| PricePoint {
                ts: p.ts,
                median_cents: p.median_cents,
                purchases: p.purchases,
            })
            .collect();
        let (spd, disp) = window_stats(&pts, 1_793_754_000 + 60).unwrap();
        assert!(spd > 3.0, "Moonrise FT vende mais de 3 por dia: {spd}");
        assert!((0.0..1.0).contains(&disp), "{disp}");
        assert_eq!(window_stats(&pts, 1_600_000_000), None);
    }

    #[test]
    fn selection_ranks_by_score_filters_by_balance_and_sales() {
        let items = vec![
            item("A (Field-Tested)", 400, 1, 20.0, 0.10), // 22,00 BRL, nota 2,0
            item("B (Field-Tested)", 400, 2, 5.0, 0.50),  // nota 2,5 → à frente de A
            item("C (Field-Tested)", 400, 3, 2.0, 0.90),  // poucas vendas
            item("D (Field-Tested)", 900, 4, 50.0, 0.50), // 49,50 BRL > saldo
            item("E (Field-Tested)", 400, 5, 10.0, 0.0),  // dispersão 0 → piso 2 %
        ];
        let tasks = select(&items, 5.5, 3608, &HashMap::new());
        let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "d:B (Field-Tested)",
                "d:A (Field-Tested)",
                "d:E (Field-Tested)"
            ]
        );
        assert_eq!(tasks[0].wear, Some(2));
        // acertos recentes sobem a posição
        let hits = HashMap::from([("d:E (Field-Tested)".to_string(), 30)]);
        let ids: Vec<String> = select(&items, 5.5, 3608, &hits)
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids[0], "d:E (Field-Tested)");
        // saldo menor tira quem não cabe (22,00 > 20,00)
        assert!(select(&items, 5.5, 2000, &HashMap::new()).is_empty());
    }

    #[test]
    fn selection_caps_at_watch_max() {
        let items: Vec<Item> = (0..100)
            .map(|i| item(&format!("S{i} (Field-Tested)"), 300, i, 5.0 + i as f64, 0.2))
            .collect();
        assert_eq!(
            select(&items, 5.0, 100_000, &HashMap::new()).len(),
            WATCH_MAX
        );
    }

    #[test]
    fn rare_tasks_come_from_seed_tables_and_floor_within_balance() {
        let moon = "Glock-18 | Moonrise (Field-Tested)";
        let items = vec![
            Item {
                code: Some("G184020E2063004".into()),
                ..item(moon, 120, 1, 0.0, 0.0) // sem vendas/dia: não vira desconto, mas vira raro
            },
            item("AK-47 | Redline (Field-Tested)", 300, 2, 0.0, 0.0),
        ];
        let tasks = select(&items, 5.0, 3608, &HashMap::new());
        assert_eq!(tasks.len(), seed_ranges(moon).len());
        let t = &tasks[0];
        assert_eq!(t.id, "r:G184020E2063004:58-59");
        assert_eq!(
            (t.seeds, t.wear, t.name.as_str()),
            (Some((58, 59)), None, "Glock-18 | Moonrise")
        );
        // piso 6,00 > saldo 5,00 → nenhuma consulta rara
        assert!(select(&items, 5.0, 500, &HashMap::new()).is_empty());
    }
}
