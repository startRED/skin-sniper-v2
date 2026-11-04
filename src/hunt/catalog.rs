//! Descoberta e seleção do que vigiar dado o saldo (docs/projeto.md, "Caça" 1–2).
//!
//! Um ciclo, um passo por vez (cada passo é uma consulta à Steam, no ritmo do limitador):
//! 1. varredura de `search/render` por popularidade, uma página por categoria por rodada (cache de 24 h);
//! 2. razão BRL/USD medida entre um anúncio em BRL e o preço USD do mesmo item (nunca fixa no código);
//! 3. `QueryPriceHistory` dos candidatos com piso ≤ saldo (cache de 6 h);
//! 4. seleção por nota = vendas/dia × lucro possível × (1 + acertos), mais uma consulta de grupo raro por
//!    skin com tabela × desgaste; raros ficam com metade das consultas. Tudo precisa caber no saldo.
//!
//! shortcut: a varredura cobre `SCAN_ROUNDS` páginas (60 itens) por categoria, não o catálogo inteiro (cada
//! página custa uma consulta e há milhares); subir se faltarem candidatos para o saldo.

use super::scheduler::Task;
use super::{Hunt, detect};
use crate::db::Db;
use crate::pricing::fees::seller_gets;
use crate::pricing::groups::seed_ranges;
use crate::pricing::sell::{
    MIN_LIST_MARGIN_PCT, MIN_LIST_PROFIT_CENTS, PricePoint, weighted_quantile,
};
use crate::steam::client::Error as SteamError;
use crate::steam::listings::{self, paint_index_from_code};
use crate::steam::search;
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

pub const SCAN_ROUNDS: u32 = 6;
pub const HISTORY_TTL: i64 = 6 * 3600;
pub const CATALOG_TTL: i64 = 24 * 3600;
pub const MIN_SALES_PER_DAY: f64 = 3.0;
pub const WATCH_MAX: usize = 40;
pub const RARE_MAX_TASKS: usize = 24;
/// Piso visto numa consulta vale por tanto tempo (depois a tarefa é conferida de novo).
pub const FLOOR_TTL: i64 = 6 * 3600;
/// A seleção é refeita a cada tanto, para o piso visto tirar quem não cabe no saldo.
pub const RESELECT_SECS: i64 = 900;
/// Quanto o piso pode ficar abaixo da mediana de vendas e ainda ser plausível: a hipótese que dá o piso mínimo
/// de desconto (`min_discount_floor`) antes de haver histórico. Com histórico vale a dispersão do item.
const MAX_UPLIFT_PCT: i64 = 30;
/// Teto da dispersão usada no lucro possível: acima disso o histórico é ruído, não folga.
const MAX_DISPERSION: f64 = 0.5;
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

/// Lucro mínimo para entrar na lista a esse preço de compra: R$ 0,20 e 5 % (as mesmas regras de `sell::evaluate`).
pub fn min_profit(price_cents: i64) -> i64 {
    let by_margin = (price_cents as f64 * MIN_LIST_MARGIN_PCT / 100.0).ceil() as i64;
    MIN_LIST_PROFIT_CENTS.max(by_margin)
}

/// Lucro possível de comprar no piso e vender na mediana: o piso fica `dispersão` abaixo do P50, então a venda
/// é piso ÷ (1 − dispersão) e o vendedor recebe `seller_gets` disso (taxa mínima de 1 + 1 centavo incluída).
pub fn possible_profit(floor_cents: i64, dispersion: f64) -> i64 {
    let d = dispersion.clamp(0.0, MAX_DISPERSION);
    let sell = (floor_cents as f64 / (1.0 - d)).round() as i64;
    seller_gets(sell) - floor_cents
}

/// Menor piso (centavos) em que uma venda `MAX_UPLIFT_PCT` % acima do piso ainda dá o lucro mínimo.
/// Abaixo disso a taxa come tudo; deriva da fórmula de taxa, não de um número solto.
pub fn min_discount_floor() -> i64 {
    (1..)
        .find(|&p| seller_gets(p * (100 + MAX_UPLIFT_PCT) / 100) - p >= min_profit(p))
        .unwrap_or(i64::MAX)
}

/// O que as consultas já viram de uma tarefa.
#[derive(Debug, Default, PartialEq)]
pub struct Observed {
    /// Menor piso entre as consultas recentes que viram anúncio.
    pub floor: Option<i64>,
    /// Hora da consulta mais recente (qualquer idade).
    pub at: Option<i64>,
    /// Quantas chaves (faixas, para raro) têm consulta recente.
    pub fresh: usize,
    /// Dessas, quantas não viram anúncio nenhum.
    pub empty: usize,
}

/// Junta os pisos guardados por `Task::floor_key` (`<id>` ou `<id>#<de>-<até>`).
pub fn observed(floors: &HashMap<String, (Option<i64>, i64)>, id: &str, now: i64) -> Observed {
    let prefix = format!("{id}#");
    let mut o = Observed::default();
    for (_, &(f, at)) in floors
        .iter()
        .filter(|(k, _)| k.as_str() == id || k.starts_with(&prefix))
    {
        o.at = o.at.max(Some(at));
        if now - at < FLOOR_TTL {
            o.fresh += 1;
            o.empty += usize::from(f.is_none());
            o.floor = match (o.floor, f) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
    }
    o
}

/// Nota de um item: vendas/dia × lucro possível em reais (≈ lucro/dia se cada venda desse dinheiro). Sem lucro
/// possível pelo histórico o item não sai, só fica no fim: um anúncio fora da curva ainda pode dar lucro.
pub fn score(sales_per_day: f64, possible_profit_cents: i64) -> f64 {
    sales_per_day * (possible_profit_cents.max(0) + 1) as f64 / 100.0
}

/// Mudou mais de 20 % (ou não havia saldo antes).
pub fn saldo_moved(old: i64, new: i64) -> bool {
    old <= 0 || (new - old).abs() * 100 > old * 20
}

/// Tudo de que a seleção depende além dos itens.
pub struct Ctx<'a> {
    pub ratio: f64,
    pub saldo: i64,
    pub now: i64,
    pub hits: &'a HashMap<String, u32>,
    /// `Shared::floors`: pisos vistos nas consultas.
    pub floors: &'a HashMap<String, (Option<i64>, i64)>,
}

/// Desconto: itens com piso entre o mínimo viável e o saldo, ≥ 3 vendas/dia e lucro possível ≥ mínimo, por nota
/// (até `WATCH_MAX`). Raro: uma tarefa por skin com tabela × desgaste cujo piso cabe no saldo (até
/// `RARE_MAX_TASKS`), com metade do peso das consultas. O piso é o do anúncio visto numa consulta recente;
/// sem consulta, USD × razão (que só serve de triagem).
pub fn select(items: &[Item], c: &Ctx) -> Vec<Task> {
    let ratio_floor = |i: &Item| i.floor_usd_cents.map(|u| usd_to_brl(u, c.ratio));
    let min_floor = min_discount_floor();
    let mut common: Vec<(f64, Task)> = items
        .iter()
        .filter_map(|i| {
            let code = i.code.clone()?;
            let id = format!("d:{}", i.name);
            let f = observed(c.floors, &id, c.now).floor.or(ratio_floor(i))?;
            let spd = i.sales_per_day.filter(|s| *s >= MIN_SALES_PER_DAY)?;
            let profit = possible_profit(f, i.dispersion.unwrap_or(0.0));
            if f > c.saldo || f < min_floor {
                return None;
            }
            let base = score(spd, profit);
            let with_hits = base * (1.0 + f64::from(c.hits.get(&id).copied().unwrap_or(0)));
            Some((
                with_hits,
                Task {
                    id,
                    name: i.name.clone(),
                    code,
                    wear: wear_of(&i.name),
                    seeds: vec![],
                    score: base,
                },
            ))
        })
        .collect();
    common.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    common.truncate(WATCH_MAX);
    let total: f64 = common.iter().map(|c| c.1.score).sum();
    let mut tasks: Vec<Task> = common.into_iter().map(|c| c.1).collect();

    // Raros: skin com tabela × desgaste, uma vez só (itens repetidos do catálogo não duplicam).
    let mut by_wear: BTreeMap<(&str, u8), (i64, &str)> = BTreeMap::new();
    for i in items {
        let (Some(code), Some(wear)) = (i.code.as_deref(), wear_of(&i.name)) else {
            continue;
        };
        let f = ratio_floor(i).unwrap_or(0);
        let plain = !i.name.contains("StatTrak™") && !i.name.starts_with("Souvenir");
        if plain && f <= c.saldo && !seed_ranges(&i.name).is_empty() {
            by_wear.entry((code, wear)).or_insert((f, i.name.as_str()));
        }
    }
    let mut rare: Vec<(i64, Task)> = by_wear
        .into_iter()
        .filter_map(|((code, wear), (f, name))| {
            let id = format!("r:{code}:{wear}");
            let seeds = seed_ranges(name);
            let seen = observed(c.floors, &id, c.now);
            // todas as faixas conferidas, todas com anúncio e o menor não cabe: nada a caçar aqui agora
            let out = seen.fresh >= seeds.len()
                && seen.empty == 0
                && seen.floor.is_some_and(|f| f > c.saldo);
            (!out).then(|| {
                let task = Task {
                    id,
                    name: name.to_string(),
                    code: code.to_string(),
                    wear: Some(wear),
                    seeds,
                    score: 0.0,
                };
                (seen.floor.unwrap_or(f), task)
            })
        })
        .collect();
    rare.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.id.cmp(&b.1.id)));
    rare.truncate(RARE_MAX_TASKS);
    // Metade das consultas para raros: o peso somado deles iguala o dos comuns.
    let share = if total > 0.0 { total } else { 10.0 } / rare.len().max(1) as f64;
    tasks.extend(rare.into_iter().map(|(_, t)| Task { score: share, ..t }));
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
    let min_floor = min_discount_floor();
    Ok(items
        .into_iter()
        .filter(|i| {
            i.code.is_some()
                && i.floor_usd_cents
                    .is_some_and(|u| (min_floor..=saldo).contains(&usd_to_brl(u, ratio)))
                && i.history_at.is_none_or(|t| now - t >= HISTORY_TTL)
                && !exclude.contains(&i.name)
        })
        .take(HISTORY_PER_ROUND)
        .map(|i| i.name)
        .collect())
}

// ---------------------------------------------------------------------------------------------
// Seleção

/// Reseleciona se o saldo mudou mais de 20 % ou passaram 15 min desde a última seleção.
pub async fn maybe_reselect(h: &Hunt) -> Result<()> {
    let now = h.now();
    let due = {
        let s = h.lock();
        s.ratio.is_some()
            && s.selected
                .is_none_or(|(sal, t)| saldo_moved(sal, s.saldo) || now - t >= RESELECT_SECS)
    };
    if due { reselect_now(h).await } else { Ok(()) }
}

pub async fn reselect_now(h: &Hunt) -> Result<()> {
    let now = h.now();
    let (saldo, ratio, floors) = {
        let s = h.lock();
        (s.saldo, s.ratio, s.floors.clone())
    };
    let Some((ratio, _)) = ratio else {
        return Ok(());
    };
    let items = load_items(&h.db).await?;
    let hits = recent_hits(&h.db, now).await?;
    let ctx = Ctx {
        ratio,
        saldo,
        now,
        hits: &hits,
        floors: &floors,
    };
    let tasks = select(&items, &ctx);
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

    const NO_HITS: fn() -> HashMap<String, u32> = HashMap::new;

    fn ctx<'a>(
        saldo: i64,
        hits: &'a HashMap<String, u32>,
        floors: &'a HashMap<String, (Option<i64>, i64)>,
    ) -> Ctx<'a> {
        Ctx {
            ratio: 5.5,
            saldo,
            now: 1_793_770_000,
            hits,
            floors,
        }
    }

    /// Catálogo do dia 04/11/2026 (ver `_source` no arquivo) e o saldo que o Erick tinha.
    fn catalog_fixture() -> (Vec<Item>, i64) {
        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/testdata/selection_catalog_2026-11-04.json"
            ))
            .unwrap(),
        )
        .unwrap();
        let items = v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| Item {
                name: i["name"].as_str().unwrap().into(),
                code: i["code"].as_str().map(Into::into),
                floor_usd_cents: i["floor_usd_cents"].as_i64(),
                rank: i["rank"].as_i64().unwrap(),
                sales_per_day: i["sales_per_day"].as_f64(),
                dispersion: i["dispersion"].as_f64(),
                history_at: i["history_at"].as_i64(),
            })
            .collect();
        (items, v["saldo_cents"].as_i64().unwrap())
    }

    #[test]
    fn minimum_floor_comes_from_the_fee_formula() {
        // abaixo do mínimo nem a taxa de 1 + 1 centavo deixa R$ 0,20 de lucro; acima, deixa
        let min = min_discount_floor();
        assert!((140..=200).contains(&min), "{min}");
        let ok = |p: i64| seller_gets(p * (100 + MAX_UPLIFT_PCT) / 100) - p >= min_profit(p);
        assert!(ok(min) && !ok(min - 1));
        // preço de R$ 0,19 com dispersão de 50 %: vende a R$ 0,38 e a taxa leva quase tudo
        assert!(possible_profit(19, 0.5) < MIN_LIST_PROFIT_CENTS);
        // R$ 10 com 20 % de dispersão (venda a R$ 12,50) sobra mais que o mínimo
        assert_eq!(possible_profit(1000, 0.2), 1088 - 1000);
        assert_eq!(min_profit(1000), 50);
        assert_eq!(min_profit(100), 20);
    }

    #[test]
    fn score_rewards_absolute_profit_not_just_sales() {
        // mesma dispersão: o item de R$ 20 rende mais lucro por venda que o de R$ 2, mesmo vendendo menos
        let (cheap, dear) = (possible_profit(200, 0.3), possible_profit(2000, 0.3));
        assert!(score(100.0, cheap) < score(50.0, dear));
        assert_eq!(score(10.0, -5), 0.1); // sem lucro possível: só o desempate de 1 centavo
    }

    #[test]
    fn selection_with_36_reais_skips_junk_and_unaffordable() {
        let (items, saldo) = catalog_fixture();
        assert_eq!(saldo, 3608);
        let floors = HashMap::new();
        let hits = NO_HITS();
        let tasks = select(&items, &ctx(saldo, &hits, &floors));
        let (rare, common): (Vec<&Task>, Vec<&Task>) = tasks.iter().partition(|t| t.rare());
        let min = min_discount_floor();
        for t in &common {
            let i = items.iter().find(|i| i.name == t.name).unwrap();
            let f = usd_to_brl(i.floor_usd_cents.unwrap(), 5.5);
            assert!((min..=saldo).contains(&f), "{} piso {f}", t.name);
        }
        let names: HashSet<&str> = common.iter().map(|t| t.name.as_str()).collect();
        assert!(
            !names.contains("M249 | Sage Camo (Factory New)"),
            "piso de R$ 0,18"
        );
        assert!(
            !names.contains("AK-47 | Fuel Injector (Field-Tested)"),
            "acima do saldo"
        );
        assert!(names.contains("AWP | Duality (Field-Tested)"));
        // raros: uma tarefa por skin × desgaste que cabe, sem repetir e sem os caros
        let ids: Vec<&str> = rare.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            ids.len(),
            HashSet::<&&str>::from_iter(ids.iter()).len(),
            "repetido: {ids:?}"
        );
        let rn: Vec<&str> = rare.iter().map(|t| t.name.as_str()).collect();
        for want in [
            "Glock-18 | Moonrise (Field-Tested)",
            "Glock-18 | Moonrise (Well-Worn)",
            "Glock-18 | Moonrise (Battle-Scarred)",
            "Desert Eagle | Heat Treated (Field-Tested)",
            "Five-SeveN | Case Hardened (Field-Tested)",
        ] {
            assert!(rn.contains(&want), "faltou {want}: {rn:?}");
        }
        for no in [
            "Glock-18 | Fade (Factory New)",
            "AK-47 | Case Hardened (Field-Tested)",
        ] {
            assert!(!rn.contains(&no), "não cabe no saldo: {no}");
        }
        assert!(rare.iter().all(|t| !t.name.contains("StatTrak")));
        let moon = rare
            .iter()
            .find(|t| t.name == "Glock-18 | Moonrise (Field-Tested)")
            .unwrap();
        assert_eq!(
            (moon.wear, moon.seeds.len()),
            (Some(2), seed_ranges(&moon.name).len())
        );
    }

    #[test]
    fn rare_floor_seen_above_balance_drops_the_task_until_it_gets_stale() {
        let (items, saldo) = catalog_fixture();
        let hit = NO_HITS();
        let heat = "Desert Eagle | Heat Treated (Field-Tested)";
        let id_of = |tasks: &[Task]| {
            tasks
                .iter()
                .find(|t| t.rare() && t.name == heat)
                .map(|t| t.id.clone())
        };
        let id = id_of(&select(&items, &ctx(saldo, &hit, &HashMap::new()))).unwrap();
        let n = seed_ranges(heat).len();
        let mut floors: HashMap<String, (Option<i64>, i64)> = (0..n)
            .map(|k| (format!("{id}#{k}-{k}"), (Some(28_000), 1_793_769_000)))
            .collect();
        // todas as faixas conferidas, todas caras: sai
        assert_eq!(id_of(&select(&items, &ctx(saldo, &hit, &floors))), None);
        // uma faixa ainda sem consulta (ou vazia): continua
        floors.remove(&format!("{id}#0-0"));
        assert!(id_of(&select(&items, &ctx(saldo, &hit, &floors))).is_some());
        floors.insert(format!("{id}#0-0"), (None, 1_793_769_000));
        assert!(id_of(&select(&items, &ctx(saldo, &hit, &floors))).is_some());
        // uma faixa barata: continua
        floors.insert(format!("{id}#0-0"), (Some(900), 1_793_769_000));
        assert!(id_of(&select(&items, &ctx(saldo, &hit, &floors))).is_some());
        // consulta velha não vale: volta a ser conferida
        floors.insert(format!("{id}#0-0"), (Some(28_000), 1_793_769_000));
        let mut late = ctx(saldo, &hit, &floors);
        late.now += FLOOR_TTL;
        assert!(id_of(&select(&items, &late)).is_some());
    }

    #[test]
    fn common_floor_seen_above_balance_drops_the_task() {
        let (items, saldo) = catalog_fixture();
        let hit = NO_HITS();
        let name = "AWP | Duality (Field-Tested)";
        let has = |floors: &HashMap<_, _>| {
            select(&items, &ctx(saldo, &hit, floors))
                .iter()
                .any(|t| t.name == name)
        };
        assert!(has(&HashMap::new()));
        let seen = HashMap::from([(format!("d:{name}"), (Some(saldo + 1), 1_793_769_000))]);
        assert!(!has(&seen), "o anúncio de verdade custa mais que o saldo");
    }

    #[test]
    fn rare_tasks_take_half_of_the_queries() {
        let (items, saldo) = catalog_fixture();
        let floors = HashMap::new();
        let hit = NO_HITS();
        let tasks = select(&items, &ctx(saldo, &hit, &floors));
        let mut q = super::super::scheduler::Queue::default();
        q.set_tasks(tasks, &HashMap::new(), 0);
        let (mut rare, mut now) = (0, 0);
        for _ in 0..1200 {
            let t = q.pick().unwrap();
            q.done(&t.id, now);
            rare += i32::from(t.rare());
            now += 3_000;
        }
        let share = f64::from(rare) / 1200.0;
        assert!((0.4..=0.6).contains(&share), "parcela de raros {share}");
    }

    #[test]
    fn selection_caps_and_ranks_commons() {
        let items: Vec<Item> = (0..100)
            .map(|i| item(&format!("S{i} (Field-Tested)"), 300, i, 5.0 + i as f64, 0.3))
            .collect();
        let (floors, hits) = (HashMap::new(), NO_HITS());
        let tasks = select(&items, &ctx(100_000, &hits, &floors));
        assert_eq!(tasks.len(), WATCH_MAX);
        // mais vendas, mais nota; acertos recentes sobem
        assert_eq!(tasks[0].id, "d:S99 (Field-Tested)");
        let hits = HashMap::from([("d:S0 (Field-Tested)".to_string(), 1000)]);
        let tasks = select(&items, &ctx(100_000, &hits, &floors));
        assert_eq!(tasks[0].id, "d:S0 (Field-Tested)");
    }
}
