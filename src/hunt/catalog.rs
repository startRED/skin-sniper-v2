//! Descoberta e seleção do que vigiar dado o saldo (docs/projeto.md, "Caça" 1–2).
//!
//! Um ciclo, um passo por vez (cada passo é uma consulta à Steam, no ritmo do limitador):
//! 1. varredura de `search/render` por popularidade, uma página por categoria por rodada (cache de 24 h), e uma
//!    busca por nome de cada skin com tabela de pattern (raras ficam longe do topo da popularidade);
//! 2. razão BRL/USD medida entre um anúncio em BRL e o preço USD do mesmo item (nunca fixa no código);
//! 3. `QueryPriceHistory` dos candidatos com piso ≤ saldo (cache de 6 h);
//! 4. seleção por nota = vendas/dia × lucro possível × (1 + acertos), mais uma consulta de grupo raro por
//!    skin com tabela (todos os desgastes numa consulta). A nota só escolhe quem
//!    entra e desempata o que ainda não foi testado; quem recebe consultas é o bandido (`scheduler`). Tudo precisa
//!    caber no saldo.
//!
//! shortcut: a varredura cobre `SCAN_ROUNDS` páginas do topo e `TAIL_ROUNDS` amostras da cauda por categoria,
//! não o catálogo inteiro (cada página custa uma consulta e há milhares); subir se faltarem candidatos para o saldo.

use super::scheduler::Task;
use super::{Hunt, detect};
use crate::db::Db;
use crate::pricing::fees::seller_gets;
use crate::pricing::groups::{seed_ranges, table_skins};
use crate::pricing::sell::{
    MIN_LIST_MARGIN_PCT, MIN_LIST_PROFIT_CENTS, PricePoint, weighted_quantile,
};
use crate::steam::client::{Endpoint, Error as SteamError};
use crate::steam::listings::{self, paint_index_from_code};
use crate::steam::search;
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

pub const SCAN_ROUNDS: u32 = 6;
/// Rodadas depois do topo: uma página por faixa de popularidade que dobra (60–120, 120–240, … 1920–3840), outra
/// a cada dia. É lá que ficam as de giro lento, com mais folga de preço que as de centenas de vendas/dia.
pub const TAIL_ROUNDS: u32 = 6;
pub const HISTORY_TTL: i64 = 6 * 3600;
pub const CATALOG_TTL: i64 = 24 * 3600;
pub const MIN_SALES_PER_DAY: f64 = 1.0;
/// Acima disso vender mais rápido não ajuda quem compra uma unidade: a nota passa a ser só o lucro.
const SCORE_SALES_CAP: f64 = 10.0;
pub const WATCH_MAX: usize = 40;
pub const RARE_MAX_TASKS: usize = 24;
/// Rank das entradas vindas da busca por nome de skin com tabela (depois da popularidade); não rebaixa o rank
/// que a varredura já deu ao item.
const SKIN_RANK_BASE: i64 = 100_000;
/// Piso visto numa consulta vale por tanto tempo (depois a tarefa é conferida de novo).
pub const FLOOR_TTL: i64 = 6 * 3600;
/// A seleção é refeita a cada tanto, para o piso visto tirar quem não cabe no saldo.
pub const RESELECT_SECS: i64 = 900;
/// Quanto o piso pode ficar abaixo da mediana de vendas e ainda ser plausível: a hipótese que dá o piso mínimo
/// de desconto (`min_discount_floor`) antes de haver histórico. Com histórico vale a dispersão do item.
const MAX_UPLIFT_PCT: i64 = 30;
/// Peso (frente a 1,0) de um raro cujas faixas vistas até agora custam mais que o saldo.
const OVER_BUDGET_WEIGHT: f64 = 0.25;
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
/// O sufixo muda quando a varredura muda de forma: uma leitura feita pela forma antiga não vale e é refeita.
pub(super) const SCAN_KEY: &str = "catalog_scan_completa";
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

/// Início da página da rodada: o topo em sequência, depois uma página da faixa da cauda que gira com o dia.
/// A faixa termina no total da categoria (quando já visto); faixa toda além dele não gasta consulta (`None`).
pub fn scan_start(round: u32, day: i64, total: Option<u32>) -> Option<u32> {
    let (lo, hi) = if round < SCAN_ROUNDS {
        (round * 10, round * 10 + 10)
    } else {
        let base = (SCAN_ROUNDS * 10) << (round - SCAN_ROUNDS).min(20);
        (base, 2 * base)
    };
    let pages = hi
        .min(total.unwrap_or(u32::MAX))
        .saturating_sub(lo)
        .div_ceil(10);
    (pages > 0).then(|| lo + day.rem_euclid(i64::from(pages)) as u32 * 10)
}

/// Lucro mínimo para entrar na lista a esse preço de compra: R$ 1,00 e 10 % (as mesmas regras de `sell::evaluate`).
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

/// Nota de um item: vendas/dia (até `SCORE_SALES_CAP`) × lucro possível em reais. Sem lucro
/// possível pelo histórico o item não sai, só fica no fim: um anúncio fora da curva ainda pode dar lucro.
pub fn score(sales_per_day: f64, possible_profit_cents: i64) -> f64 {
    sales_per_day.min(SCORE_SALES_CAP) * (possible_profit_cents.max(0) + 1) as f64 / 100.0
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

/// Desconto: itens com piso entre o mínimo viável e o saldo, ≥ 1 venda/dia e lucro possível ≥ mínimo, por nota
/// (até `WATCH_MAX`). Raro: uma tarefa por skin com tabela (todos os desgastes) cujo piso cabe no saldo (até
/// `RARE_MAX_TASKS`, os que mais vendem primeiro), com nota-base somada igual à dos comuns. O piso é o do anúncio visto numa consulta recente;
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
    // Raros: uma tarefa por skin com tabela (código G), sem filtro de desgaste: a mesma consulta traz todos os
    // desgastes, StatTrak e Souvenir da faixa de seed, então cada faixa é revista ~4× mais vezes no mesmo ritmo.
    // Nome = o desgaste que mais vende (painel e CSFloat); piso = o menor dos desgastes; vendas = a soma.
    let mut by_code: BTreeMap<&str, (i64, &str, f64, f64)> = BTreeMap::new();
    let mut seen_names: HashSet<&str> = HashSet::new();
    for i in items {
        let Some(code) = i.code.as_deref() else {
            continue;
        };
        let f = ratio_floor(i).unwrap_or(0);
        let plain = !i.name.contains("StatTrak™") && !i.name.starts_with("Souvenir");
        if plain
            && wear_of(&i.name).is_some()
            && f <= c.saldo
            && seen_names.insert(i.name.as_str())
            && !seed_ranges(&i.name).is_empty()
        {
            let spd = i.sales_per_day.unwrap_or(MIN_SALES_PER_DAY);
            let e = by_code
                .entry(code)
                .or_insert((f, i.name.as_str(), spd, 0.0));
            e.0 = e.0.min(f);
            if spd > e.2 {
                (e.1, e.2) = (i.name.as_str(), spd);
            }
            e.3 += spd;
        }
    }
    let mut rare: Vec<(f64, i64, Task, f64)> = by_code
        .into_iter()
        .filter_map(|(code, (f, name, _, spd))| {
            let id = format!("r:{code}");
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
                    wear: None,
                    seeds,
                    score: 0.0,
                };
                // faixa vista acima do saldo: ainda gira (outra faixa pode estar barata), mas com menos peso
                let weight = if seen.floor.is_some_and(|f| f > c.saldo) {
                    OVER_BUDGET_WEIGHT
                } else {
                    1.0
                };
                (spd, seen.floor.unwrap_or(f), task, weight)
            })
        })
        .collect();
    // Quem mais vende primeiro: mais anúncios novos por hora (mais chance de seed rara) e revenda mais rápida.
    // O único achado real (Moonrise FT, ~300 vendas/dia) veio do topo; skins de 0–1 venda/dia ocupavam vagas.
    rare.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.id.cmp(&b.2.id))
    });
    rare.truncate(RARE_MAX_TASKS);
    // Nota-base dos raros: soma igual à dos comuns (só desempata braços ainda não testados).
    let per_weight =
        if total > 0.0 { total } else { 10.0 } / rare.iter().map(|r| r.3).sum::<f64>().max(1e-9);
    tasks.extend(rare.into_iter().map(|(_, _, t, w)| Task {
        score: per_weight * w,
        ..t
    }));
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
    /// `total_count` de cada categoria, visto na última página dela.
    totals: HashMap<search::Category, u32>,
    /// Skins com tabela ainda sem busca por nome neste ciclo.
    skins_left: VecDeque<String>,
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
            skins_left: if scanning {
                table_skins().into()
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
    Skin(String),
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
                if let Ok(total) = r {
                    h.lock().disc.totals.insert(cat, total);
                }
                finish_scan(h, cat, r.map(|_| ()))?;
                return Ok(true);
            }
            Next::Skin(name) => {
                let r = scan_skin(h, &name).await;
                if r.as_ref().err().is_some_and(retryable) {
                    h.lock().disc.skins_left.push_front(name);
                }
                r?;
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
        ScanDone,
    }
    let pre = {
        let mut s = h.lock();
        let ratio_known = s.ratio.is_some();
        let d = &mut s.disc;
        let day = now.div_euclid(DAY);
        let scan = std::iter::from_fn(|| d.scan_left.pop_front()).find_map(|cat| {
            scan_start(d.round, day, d.totals.get(&cat).copied()).map(|s| (cat, s))
        });
        if let Some((cat, start)) = scan {
            Pre::Done(Next::Scan(cat, start))
        } else if let Some(name) = d.skins_left.pop_front() {
            Pre::Done(Next::Skin(name))
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
        } else if d.scanning && d.round + 1 < SCAN_ROUNDS + TAIL_ROUNDS {
            Pre::Advance
        } else if d.scanning {
            Pre::ScanDone
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
        // Só o fim de todas as rodadas marca a varredura: marcar no fim da 1ª fazia um reinício no meio
        // pular o resto (topo fundo e cauda) por 24 h. Se a gravação falhar, o próximo passo tenta de novo.
        Pre::ScanDone => {
            set_setting(&h.db, SCAN_KEY, h.now().to_string()).await?;
            h.lock().disc.scanning = false;
            Box::pin(next_step(h, now)).await
        }
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

/// Grava a página e devolve o `total_count` da categoria.
async fn scan_page(h: &Hunt, cat: search::Category, start: u32) -> Result<u32> {
    let page = search::fetch_page(&h.steam, cat, start).await?;
    store_entries(h, page.entries, i64::from(start)).await?;
    Ok(page.total_count)
}

/// Busca por nome de uma skin com tabela: traz todos os desgastes com o código G e o piso em USD. Falha
/// de parse não trava o ciclo (a skin só fica sem tarefa); erro de rede sobe.
async fn scan_skin(h: &Hunt, base: &str) -> Result<()> {
    let q = [
        // a Valve não acha o nome com o " | "; sem ele acha (todos os desgastes e StatTrak)
        ("query", base.replace(" | ", " ")),
        ("start", "0".into()),
        ("count", "20".into()),
        ("search_descriptions", "0".into()),
        ("sort_column", "popular".into()),
        ("sort_dir", "desc".into()),
        ("appid", "730".into()),
        ("norender", "1".into()),
    ];
    let v = h
        .steam
        .get_json(Endpoint::Search, "/market/search/render/", &q)
        .await?;
    let page = match search::parse_page(&v) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("busca de {base} sem resultado legível: {e}");
            return Ok(());
        }
    };
    let entries = page
        .entries
        .into_iter()
        .filter(|e| e.name.contains(base))
        .collect();
    store_entries(h, entries, SKIN_RANK_BASE).await
}

async fn store_entries(h: &Hunt, entries: Vec<search::CatalogEntry>, rank_base: i64) -> Result<()> {
    let now = h.now();
    let ratio = h.lock().ratio.map(|r| r.0);
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
                        icon_url = excluded.icon_url, updated_at = excluded.updated_at,
                        rank = CASE WHEN excluded.rank >= ?9 THEN COALESCE(items.rank, excluded.rank) ELSE excluded.rank END",
                    params![
                        e.name,
                        e.item_code,
                        paint,
                        floor_brl,
                        e.sell_price_usd_cents,
                        e.icon_url,
                        rank_base + i as i64,
                        now,
                        SKIN_RANK_BASE
                    ],
                )?;
            }
            tx.commit()
        })
        .await?;
    Ok(())
}

/// Depois de uma página: erro não retentável não trava a rodada.
fn finish_scan(h: &Hunt, cat: search::Category, r: Result<()>) -> Result<()> {
    if let Err(e) = r {
        if retryable(&e) {
            h.lock().disc.scan_left.push_front(cat); // a página volta à frente da fila
        }
        return Err(e);
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
        let removed = s.queue.set_tasks(tasks);
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

pub(crate) async fn get_setting(db: &Db, key: &'static str) -> Result<Option<String>> {
    db.call(move |c| {
        c.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()
    })
    .await
}

pub(crate) async fn set_setting(db: &Db, key: &'static str, value: String) -> Result<()> {
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
            .map(|i| {
                let name = i["name"].as_str().unwrap();
                // a Steam dá um código G por skin (todos os desgastes, StatTrak e Souvenir); o fixture traz um por
                // nome, então o teste agrupa pela skin-base
                let base = name
                    .trim_start_matches("StatTrak™ ")
                    .trim_start_matches("Souvenir ")
                    .split(" (")
                    .next()
                    .unwrap();
                (name, i["code"].as_str().map(|_| format!("G:{base}")), i)
            })
            .map(|(name, code, i)| Item {
                name: name.into(),
                code,
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
        // abaixo do mínimo uma venda 30 % acima não deixa R$ 1,00 e 10 % depois da taxa; acima, deixa
        let min = min_discount_floor();
        assert!((700..=800).contains(&min), "{min}");
        let ok = |p: i64| seller_gets(p * (100 + MAX_UPLIFT_PCT) / 100) - p >= min_profit(p);
        assert!(ok(min) && !ok(min - 1));
        // preço de R$ 0,19 com dispersão de 50 %: vende a R$ 0,38 e a taxa leva quase tudo
        assert!(possible_profit(19, 0.5) < MIN_LIST_PROFIT_CENTS);
        // R$ 10 com 20 % de dispersão (venda a R$ 12,50) não chega a R$ 1,00; R$ 20 com 30 % (venda a R$ 28,57) chega
        assert_eq!(possible_profit(1000, 0.2), 1088 - 1000);
        assert!(possible_profit(1000, 0.2) < min_profit(1000));
        assert!(possible_profit(2000, 0.3) >= min_profit(2000));
        assert_eq!(min_profit(1000), 100);
        assert_eq!(min_profit(2000), 200);
        assert_eq!(min_profit(100), 100);
    }

    #[test]
    fn scan_covers_the_top_then_samples_the_tail_by_day() {
        let at = |r, day| scan_start(r, day, None).unwrap();
        let top: Vec<u32> = (0..SCAN_ROUNDS).map(|r| at(r, 7)).collect();
        assert_eq!(top, [0, 10, 20, 30, 40, 50]);
        assert_eq!((at(6, 0), at(6, 5), at(6, 6)), (60, 110, 60));
        for r in SCAN_ROUNDS..SCAN_ROUNDS + TAIL_ROUNDS {
            let base = 60 << (r - SCAN_ROUNDS);
            for day in [-1, 0, 1, 20_000] {
                assert!((base..2 * base).contains(&at(r, day)));
            }
        }
        assert_ne!(at(11, 0), at(11, 1));
        // rifles com 2.947: a última faixa (1920–3840) só pede páginas que existem
        assert!((0..400).all(|d| scan_start(11, d, Some(2947)).unwrap() < 2947));
        assert_eq!(scan_start(11, 103, Some(2947)), Some(1920));
        // categoria pequena: faixa além do total não gasta consulta, nem a do topo
        assert_eq!(scan_start(9, 0, Some(450)), None);
        assert_eq!(scan_start(5, 0, Some(45)), None);
        assert_eq!(scan_start(4, 0, Some(45)), Some(40));
    }

    #[test]
    fn score_rewards_absolute_profit_not_just_sales() {
        // mesma dispersão: o item de R$ 20 rende mais lucro por venda que o de R$ 2, mesmo vendendo menos
        let (cheap, dear) = (possible_profit(200, 0.3), possible_profit(2000, 0.3));
        assert!(score(100.0, cheap) < score(50.0, dear));
        assert_eq!(score(10.0, -5), 0.1); // sem lucro possível: só o desempate de 1 centavo
        // giro de 600/dia com R$ 0,05 de folga não passa na frente de 2/dia com R$ 1,00
        assert!(score(600.0, 5) < score(2.0, 100));
        assert_eq!(score(600.0, 99), score(10.0, 99));
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
        // raros: uma tarefa por skin que cabe (todos os desgastes na mesma consulta), sem repetir e sem os caros
        let ids: Vec<&str> = rare.iter().map(|t| t.id.as_str()).collect();
        assert!(
            rare.iter()
                .all(|t| t.wear.is_none() && t.id == format!("r:{}", t.code)),
            "{ids:?}"
        );
        assert_eq!(
            ids.len(),
            HashSet::<&&str>::from_iter(ids.iter()).len(),
            "repetido: {ids:?}"
        );
        let rn: Vec<&str> = rare.iter().map(|t| t.name.as_str()).collect();
        let has = |skin: &str| rn.iter().filter(|n| n.starts_with(skin)).count();
        for want in [
            "Glock-18 | Moonrise (",
            "Desert Eagle | Heat Treated (",
            "Five-SeveN | Case Hardened (",
        ] {
            assert_eq!(has(want), 1, "{want}: {rn:?}");
        }
        for no in ["Glock-18 | Fade (", "AK-47 | Case Hardened ("] {
            assert_eq!(has(no), 0, "não cabe no saldo: {no}");
        }
        assert!(rare.iter().all(|t| !t.name.contains("StatTrak")));
        let moon = rare
            .iter()
            .find(|t| t.name.starts_with("Glock-18 | Moonrise ("))
            .unwrap();
        assert_eq!(
            (moon.wear, moon.seeds.len()),
            (None, seed_ranges(&moon.name).len())
        );
    }

    #[test]
    fn rare_tasks_that_sell_more_come_first() {
        let (mut items, saldo) = catalog_fixture();
        let spd = |name: &str, v: f64, items: &mut Vec<Item>| {
            items
                .iter_mut()
                .find(|i| i.name == name)
                .unwrap()
                .sales_per_day = Some(v);
        };
        spd("Glock-18 | Moonrise (Battle-Scarred)", 0.2, &mut items);
        spd("Glock-18 | Moonrise (Field-Tested)", 335.0, &mut items);
        spd("Five-SeveN | Case Hardened (Field-Tested)", 3.0, &mut items);
        let hit = NO_HITS();
        let rare: Vec<String> = select(&items, &ctx(saldo, &hit, &HashMap::new()))
            .into_iter()
            .filter(|t| t.rare())
            .map(|t| t.name)
            .collect();
        // o corte em RARE_MAX_TASKS fica com o começo desta ordem (vendas somadas dos desgastes); o nome é o
        // desgaste que mais vende: o Moonrise de 0,2/dia no BS não puxa a tarefa para o fim
        assert_eq!(rare[0], "Glock-18 | Moonrise (Field-Tested)");
        assert_eq!(rare[1], "Desert Eagle | Heat Treated (Field-Tested)");
        assert!(
            !rare
                .iter()
                .any(|n| n == "Glock-18 | Moonrise (Battle-Scarred)")
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
                .find(|t| t.rare() && t.name.starts_with("Desert Eagle | Heat Treated ("))
                .map(|t| t.id.clone())
        };
        let id = id_of(&select(&items, &ctx(saldo, &hit, &HashMap::new()))).unwrap();
        let n = seed_ranges(heat).len();
        let mut floors: HashMap<String, (Option<i64>, i64)> = (0..n)
            .map(|k| (format!("{id}#{k}-{k}"), (Some(28_000), 1_793_769_000)))
            .collect();
        // todas as faixas conferidas, todas caras: sai
        assert_eq!(id_of(&select(&items, &ctx(saldo, &hit, &floors))), None);
        // uma faixa ainda sem consulta (ou vazia): continua, mas com peso menor que um raro ainda não visto
        floors.remove(&format!("{id}#0-0"));
        let tasks = select(&items, &ctx(saldo, &hit, &floors));
        let score = |skin: &str| {
            tasks
                .iter()
                .find(|t| t.rare() && t.name.starts_with(skin))
                .unwrap()
                .score
        };
        assert!(id_of(&tasks).is_some());
        assert!(score("Desert Eagle | Heat Treated (") < score("Glock-18 | Moonrise (") / 2.0);
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
    fn selected_arms_include_discounts_and_rares_only() {
        let (items, saldo) = catalog_fixture();
        let tasks = select(&items, &ctx(saldo, &NO_HITS(), &HashMap::new()));
        assert!(tasks.iter().any(|t| t.id.starts_with("d:")));
        assert!(tasks.iter().any(|t| t.id.starts_with("r:")));
        assert!(tasks.iter().all(|t| !t.id.starts_with("f:")));
    }

    #[test]
    fn slow_seller_with_real_margin_beats_crowd_of_fast_thin_ones() {
        let mut items: Vec<Item> = (0..60)
            .map(|i| item(&format!("F{i} (Field-Tested)"), 300, i, 600.0, 0.15))
            .collect();
        items.push(item("Slow (Field-Tested)", 300, 99, 1.0, 0.3));
        let tasks = select(&items, &ctx(100_000, &NO_HITS(), &HashMap::new()));
        assert!(tasks.iter().any(|t| t.id == "d:Slow (Field-Tested)"));
        // abaixo de 1 venda/dia não entra
        items.last_mut().unwrap().sales_per_day = Some(0.9);
        let tasks = select(&items, &ctx(100_000, &NO_HITS(), &HashMap::new()));
        assert!(tasks.iter().all(|t| t.id != "d:Slow (Field-Tested)"));
    }

    #[test]
    fn selection_caps_and_ranks_commons() {
        let items: Vec<Item> = (0..100)
            .map(|i| {
                item(
                    &format!("S{i} (Field-Tested)"),
                    300,
                    i,
                    5.0 + i as f64 * 0.05,
                    0.3,
                )
            })
            .collect();
        let (floors, hits) = (HashMap::new(), NO_HITS());
        let commons = |tasks: Vec<Task>| -> Vec<Task> {
            tasks
                .into_iter()
                .filter(|t| t.id.starts_with("d:"))
                .collect()
        };
        let tasks = commons(select(&items, &ctx(100_000, &hits, &floors)));
        assert_eq!(tasks.len(), WATCH_MAX);
        // mais vendas (abaixo do teto), mais nota; acertos recentes sobem
        assert_eq!(tasks[0].id, "d:S99 (Field-Tested)");
        let hits = HashMap::from([("d:S0 (Field-Tested)".to_string(), 1000)]);
        let tasks = commons(select(&items, &ctx(100_000, &hits, &floors)));
        assert_eq!(tasks[0].id, "d:S0 (Field-Tested)");
    }
}
