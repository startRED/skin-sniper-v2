//! Detecção: anúncio novo (via `listings_seen`) com preço ≤ saldo → estimativa → avaliação → `Opportunity`
//! no banco e evento SSE; anúncio que sumiu numa nova consulta da mesma tarefa → `gone`.
//! Docs/projeto.md, "Caça" 4 e "Precificação".

use super::catalog::{FLOOR_TTL, HISTORY_TTL, min_profit, usd_to_brl, wear_label, window_stats};
use super::scheduler::{Found, Task};
use super::{Active, Hunt};
use crate::notify::{Alert, NotifyError};
use crate::pricing::brl;
use crate::pricing::groups::group_of;
use crate::pricing::premium::{self, Seal};
use crate::pricing::sell::{self, Confidence as SellConfidence, PricePoint, RareEvidence};
use crate::steam::client::Error as SteamError;
use crate::steam::history;
use crate::steam::listings::{Listing, ListingsPage};
use crate::web::api::{Confidence, Event, GoneReason, Kind, Opportunity, Risk, Sell, Tier};
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::collections::{BTreeMap, HashSet};

const DAY: i64 = 86_400;
/// Com a Steam barrando o histórico, o guardado ainda serve à avaliação por este prazo.
// shortcut: 24 h é hipótese (um dia a menos na janela de 7); rever se aviso com histórico velho errar o preço.
const STALE_HISTORY_MAX: i64 = DAY;
/// Uma página vazia pode ser transitória (conhecimento 1.3): só vale como "sumiram" na segunda seguida.
const EMPTY_PAGES_TO_TRUST: u8 = 2;
/// Balanço do que foi conferido a cada 10 min, para o painel e o log.
const CYCLE_SECS: i64 = 600;
const NEAR_KEEP: usize = 3;
/// shortcut: o mapa de avaliados só esvazia quando enche; subir se a reavaliação em massa pesar.
const EVALUATED_MAX: usize = 50_000;
/// Teto de avaliações guardadas em `evaluations` (a retenção normal é de 60 dias, `db::LEARN_KEEP_DAYS`);
/// ~0,5 kB cada, então o teto segura o banco perto de 150 MB se a caça avaliar muito mais que o medido.
pub(crate) const EVALUATIONS_KEEP: i64 = 300_000;

/// Anúncio que chegou perto de virar oportunidade (lucro pode ser negativo).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Near {
    pub name: String,
    pub price_cents: i64,
    pub mid_cents: i64,
    pub quick_cents: i64,
}

#[derive(Debug, Default)]
pub(crate) struct Cycle {
    started: i64,
    evaluated: u32,
    pub(crate) pages: u32,
    pub(crate) seen: HashSet<String>,
    pub(crate) new: u32,
    pub(crate) saved_listings: u32,
    passed: u32,
    pub(crate) near: Vec<Near>,
    last: Option<String>,
    /// Recusas por motivo, para auditar o que ficou de fora (só vai para o log).
    refused: BTreeMap<&'static str, u32>,
    /// Anúncios acima do saldo: nem são avaliados; um id conta uma vez por ciclo.
    pub(crate) over_saldo: HashSet<String>,
}

/// Perda máxima, vendido como comum, para mostrar um raro cujo prêmio ainda não fechou na Steam.
const UNPROVEN_MAX_LOSS_CENTS: i64 = 50;

pub(super) fn unproven_loses_too_much(quick_profit_cents: i64) -> bool {
    quick_profit_cents < -UNPROVEN_MAX_LOSS_CENTS
}

fn profit_text(c: i64) -> String {
    if c >= 0 {
        format!("{} de lucro", brl(c))
    } else {
        format!("{} de prejuízo", brl(-c))
    }
}

impl Cycle {
    pub(crate) fn record(&mut self, passed: bool, near: Option<Near>) {
        self.evaluated += 1;
        self.passed += u32::from(passed);
        // O mesmo anúncio volta em consultas seguidas; conta uma vez só na lista dos mais perto.
        if let Some(n) = near.filter(|n| !self.near.contains(n)) {
            self.near.push(n);
            self.near.sort_by_key(|n| std::cmp::Reverse(n.mid_cents));
            self.near.truncate(NEAR_KEEP);
        }
    }

    pub(crate) fn refuse(&mut self, why: &'static str) {
        *self.refused.entry(why).or_default() += 1;
    }

    /// "sem_lucro=12 poucas_vendas=3 acima_do_saldo=40", para o log.
    pub(crate) fn refused_text(&self) -> String {
        let over = self.over_saldo.len() as u32;
        self.refused
            .iter()
            .map(|(k, v)| (*k, *v))
            .chain((over > 0).then_some(("acima_do_saldo", over)))
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn text(&self, secs: i64) -> String {
        let min = (secs / 60).max(1);
        let listings = if self.seen.len() == 1 {
            "anúncio único"
        } else {
            "anúncios únicos"
        };
        let fresh = if self.new == 1 { "novo" } else { "novos" };
        format!(
            "{min} min · {} {listings} · {} {fresh}",
            self.seen.len(),
            self.new
        )
    }

    /// Último balanço fechado; antes do primeiro, o parcial.
    pub(crate) fn message(&self, now: i64) -> Option<String> {
        self.last.clone().or_else(|| {
            (self.pages > 0 || self.evaluated > 0).then(|| self.text(now - self.started))
        })
    }
}

/// Fecha o ciclo de 10 min: guarda o texto do painel e loga os 3 que chegaram mais perto.
pub(crate) fn roll_cycle(h: &Hunt) {
    let now = h.now();
    let mut s = h.lock();
    if s.cycle.started == 0 {
        s.cycle.started = now;
        return;
    }
    if now - s.cycle.started < CYCLE_SECS {
        return;
    }
    let c = std::mem::take(&mut s.cycle);
    s.cycle.started = now;
    let text = c.text(now - c.started);
    s.cycle.last = Some(text.clone());
    drop(s);
    tracing::info!(
        paginas = c.pages,
        distintos = c.seen.len(),
        novos = c.new,
        avaliados = c.evaluated,
        passaram = c.passed,
        consultas_anuncios_economizadas = c.saved_listings,
        recusas = %c.refused_text(),
        "{text}"
    );
    // Para medir o que dispara os bloqueios longos (quantidade ou tempo seguido): o robô antigo gravava cada chamada.
    let [main, extra, book] = h.steam.take_calls();
    tracing::info!(
        principal_ok = main.ok,
        principal_429 = main.limited,
        extra_ok = extra.ok,
        extra_429 = extra.limited,
        livro_ok = book.ok,
        livro_429 = book.limited,
        "Steam em 10 min"
    );
    for n in &c.near {
        tracing::info!(
            "perto: {} a {}; no preço médio {}, no rápido {}",
            n.name,
            brl(n.price_cents),
            profit_text(n.mid_cents),
            profit_text(n.quick_cents)
        );
    }
}

#[derive(Debug, Default)]
pub struct Outcome {
    /// Oportunidades novas e se vale avisar (`notify` da avaliação).
    pub found: Vec<(Opportunity, bool)>,
    pub gone: Vec<String>,
    /// O que a consulta rendeu à agenda (achados novos e a proximidade dos anúncios novos que não passaram).
    pub rewarded: Found,
}

pub(super) fn should_notify(opp: &Opportunity, min_profit: i64) -> bool {
    opp.gone_at.is_none()
        && opp.ev_profit_cents.is_some_and(|p| p > 0)
        && opp.sell.mid.profit_cents >= min_profit
}

/// Quase lucro, só para o contador do painel: faltou até R$ 0,30 ou 10 % do preço, o que for maior.
fn is_near(n: &Near) -> bool {
    let short = min_profit(n.price_cents) - n.mid_cents;
    short > 0 && short <= 30.max(n.price_cents / 10)
}

/// Baixa `QueryPriceHistory` do nome e grava em `price_points` e em `items` (vendas/dia, dispersão).
pub async fn fetch_and_store_history(h: &Hunt, name: &str) -> Result<()> {
    let now = h.now();
    {
        let mut s = h.lock();
        s.history_retry_after.retain(|_, until| *until > now);
        if s.history_retry_after.contains_key(name) {
            anyhow::bail!("histórico recusado recentemente; aguardando nova tentativa");
        }
    }
    let cookie = h.steam.cookie();
    let pts = match history::fetch_history(&h.steam, name).await {
        Ok(pts) => pts,
        Err(e) => {
            let mut s = h.lock();
            if matches!(e, SteamError::Http { .. } | SteamError::Parse(_))
                && h.steam.cookie() == cookie
            {
                // shortcut: recusa/JSON inválido espera 10 min; calibrar com falhas reais por nome.
                s.history_retry_after.insert(name.into(), h.now() + 600);
            }
            return Err(e.into());
        }
    };
    let now = h.now();
    let sell_pts: Vec<PricePoint> = pts.iter().map(to_sell_point).collect();
    let (spd, disp) = window_stats(&sell_pts, now).unwrap_or((0.0, 0.0));
    let name = name.to_string();
    h.db.call(move |c| {
        let tx = c.transaction()?;
        {
            let mut ins = tx.prepare(
                "INSERT OR REPLACE INTO price_points(name, ts, median_cents, purchases) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for p in &pts {
                ins.execute(params![name, p.ts, p.median_cents, p.purchases])?;
            }
        }
        tx.execute(
            "INSERT INTO items(name, updated_at, history_at, sales_per_day, dispersion) VALUES (?1, ?2, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET history_at = excluded.history_at,
                sales_per_day = excluded.sales_per_day, dispersion = excluded.dispersion",
            params![name, now, spd, disp],
        )?;
        tx.commit()
    })
    .await
}

fn to_sell_point(p: &history::PricePoint) -> PricePoint {
    PricePoint {
        ts: p.ts,
        median_cents: p.median_cents,
        purchases: p.purchases,
    }
}

async fn history_at(h: &Hunt, name: &str) -> Result<Option<i64>> {
    let n = name.to_string();
    h.db.call(move |c| {
        c.query_row("SELECT history_at FROM items WHERE name = ?1", [n], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .optional()
        .map(Option::flatten)
    })
    .await
}

/// Pontos dos últimos 30 dias do nome; baixa o histórico se passou de 6 h.
pub(crate) async fn points_for(h: &Hunt, name: &str, now: i64) -> Result<Vec<PricePoint>> {
    let n = name.to_string();
    let last: Option<i64> =
        h.db.call(move |c| {
            c.query_row("SELECT history_at FROM items WHERE name = ?1", [n], |r| {
                r.get::<_, Option<i64>>(0)
            })
            .optional()
            .map(Option::flatten)
        })
        .await?;
    if last.is_none_or(|t| now - t >= HISTORY_TTL) {
        if crate::research::current().is_some_and(|c| c.source == "paralela") {
            anyhow::bail!("histórico vencido para leitura paralela");
        }
        match fetch_and_store_history(h, name).await {
            Ok(()) => h.lock().queue.spend_discovery(),
            // Steam barrando: sem isso todo candidato fica sem avaliação enquanto o bloqueio durar.
            Err(e)
                if last.is_some_and(|t| now - t < STALE_HISTORY_MAX)
                    && matches!(
                        e.downcast_ref::<SteamError>(),
                        Some(SteamError::RateLimited { .. } | SteamError::BreakerOpen { .. })
                    ) => {}
            Err(e) => return Err(e),
        }
    }
    let n = name.to_string();
    h.db.call(move |c| {
        c.prepare(
            "SELECT ts, median_cents, purchases FROM price_points WHERE name = ?1 AND ts >= ?2 ORDER BY ts",
        )?
        .query_map(params![n, now - 30 * DAY], |r| {
            Ok(PricePoint {
                ts: r.get(0)?,
                median_cents: r.get(1)?,
                purchases: r.get(2)?,
            })
        })?
        .collect()
    })
    .await
}

/// Com menos vendas do grupo no mesmo nome que isto, valem as do grupo em todos os desgastes.
const NEAR_SALES: usize = 8;

/// Múltiplos de um grupo raro entre as vendas guardadas `(nome, float, múltiplo, grupo)`: as do grupo no mesmo
/// nome; com menos de `NEAR_SALES`, as do grupo em todos os desgastes. Só é usado para skin ainda sem medida
/// (`patterns`): o seed que paga traz a própria prova.
fn rare_multiples(
    rows: &[(String, Option<f64>, f64, Option<String>)],
    name: &str,
    group_key: &str,
) -> Vec<f64> {
    let of_group = |same_name: bool| -> Vec<f64> {
        rows.iter()
            .filter(|r| r.3.as_deref() == Some(group_key) && (!same_name || r.0 == name))
            .map(|r| r.2)
            .collect()
    };
    let same = of_group(true);
    if same.len() >= NEAR_SALES {
        same
    } else {
        of_group(false)
    }
}

/// Múltiplos `price / base_price` das vendas do CSFloat do mesmo grupo: todas as guardadas (`csfloat_sales`),
/// depois de garantir que a leitura do nome tem menos de 6 h. Só com a última leitura (40 vendas do nome) o grupo
/// tinha 5 vendas onde o banco guardava 39, e um Center Star a 1,2× o preço comum foi recusado (10/11/2026).
async fn rare_evidence(h: &Hunt, name: &str, group_key: &str) -> Result<RareEvidence> {
    let now = h.now();
    let fresh = h
        .lock()
        .csfloat_cache
        .get(name)
        .is_some_and(|(at, _)| now - at < HISTORY_TTL);
    if !fresh {
        if crate::research::current().is_some_and(|c| c.source == "paralela") {
            anyhow::bail!("vendas raras ausentes no cache da leitura paralela");
        }
        let (at, v) = match h.csfloat.sales(name).await {
            Ok(v) => {
                super::patterns::cache_sales(h, name, &v).await;
                (now, v)
            }
            Err(e) => {
                tracing::warn!("CSFloat indisponível para {name}: {e}");
                // tenta de novo em ~10 min, sem martelar
                (now - HISTORY_TTL + 600, vec![])
            }
        };
        h.lock().csfloat_cache.insert(name.to_string(), (at, v));
    }
    let (n, key) = (name.to_string(), group_key.to_string());
    let rows: Vec<(String, Option<f64>, f64, Option<String>)> =
        h.db.call(move |c| {
            c.prepare("SELECT name, \"float\", multiple, grp FROM csfloat_sales WHERE name = ?1 OR grp = ?2")?
                .query_map([n, key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect()
        })
        .await?;
    Ok(RareEvidence {
        multiples: rare_multiples(&rows, name, group_key),
    })
}

/// Múltiplos fechados na própria Steam: anúncios do mesmo nome e grupo que sumiram em até 48 h sem relistagem,
/// preço ÷ `base_cents`. O prêmio do CSFloat erra para os dois lados (medido em 11/11/2026): Center Star FT
/// previsto a 1,6× fechou a 2,0–2,6× na Steam; High Beam T1 previsto a 2,9× fechou a 1,4×.
// shortcut: sumir não é vender com certeza (retirada sem relistagem conta como venda) e anúncio barato some
// primeiro, o que puxa a mediana para baixo; se um aviso com esta prova não fechar na venda, exigir mais de
// `RARE_MIN_SALES` fechamentos ou medir a taxa de sumiço por faixa de preço.
async fn steam_closed(h: &Hunt, name: &str, group_key: &str, base_cents: i64) -> Result<Vec<f64>> {
    let (name, grp) = (name.to_string(), group_key.to_string());
    let prices: Vec<i64> =
        h.db.call(move |c| {
            c.prepare(
                "SELECT price_cents FROM observations WHERE grp = ?1 AND name = ?2 AND relisted = 0
                 AND gone_at IS NOT NULL AND gone_at - first_seen <= ?3",
            )?
            .query_map(params![grp, name, super::teacher::FAST], |r| r.get(0))?
            .collect()
        })
        .await?;
    Ok(prices
        .into_iter()
        .map(|p| p as f64 / base_cents as f64)
        .collect())
}

fn api_confidence(c: SellConfidence) -> Confidence {
    match c {
        SellConfidence::High => Confidence::Alta,
        SellConfidence::Medium => Confidence::Media,
        SellConfidence::Low => Confidence::Baixa,
    }
}

fn api_sell(s: &sell::Sell, correction: f64) -> Sell {
    let t = |t: &sell::SellTier| Tier {
        price_cents: t.price_cents,
        net_cents: t.net_cents,
        profit_cents: t.profit_cents,
        eta_hours: t.eta_hours,
    };
    Sell {
        high: t(&s.high),
        mid: t(&s.mid),
        quick: t(&s.quick),
        correction,
    }
}

fn steam_url(code: &str, l: &Listing) -> String {
    format!(
        "https://steamcommunity.com/market/listings/730/{code}?detail={}",
        l.listing_id
    )
}

/// Resultado de uma avaliação conclusiva.
struct Judged {
    /// Virou (ou segue) oportunidade; o bool é o `notify` da avaliação.
    found: Option<(Opportunity, bool)>,
    near: Option<Near>,
    /// Motivo da recusa quando não virou oportunidade.
    refused: Option<&'static str>,
    /// O porquê em texto: a evidência da estimativa ou o erro dela.
    detail: Option<String>,
    /// Compra virtual para o professor (`teacher`): virou oportunidade ou chegou perto.
    ghost: Option<super::teacher::Ghost>,
}

/// Uma linha de `evaluations`: o anúncio como veio e o que o radar decidiu.
struct EvalRow {
    listing_id: String,
    name: String,
    price_cents: i64,
    float: Option<f64>,
    seed: Option<i64>,
    paint_index: Option<i64>,
    group: Option<String>,
    stickers: String,
    keychains: String,
    inspect_url: Option<String>,
    steam_url: String,
    others_min: Option<i64>,
    mid_profit: Option<i64>,
    quick_profit: Option<i64>,
    reason: &'static str,
    detail: Option<String>,
}

impl EvalRow {
    fn new(task: &Task, l: &Listing, others_min: Option<i64>, reason: &'static str) -> EvalRow {
        EvalRow {
            listing_id: l.listing_id.clone(),
            name: l.name.clone(),
            price_cents: l.price_cents,
            float: l.float,
            seed: l.seed.map(i64::from),
            paint_index: l.paint_index.map(i64::from),
            group: group_of(&l.name, l.seed.map(u32::from), l.paint_index, l.float)
                .map(|g| g.label),
            stickers: serde_json::to_string(&l.stickers).unwrap_or_default(),
            keychains: serde_json::to_string(&l.keychains).unwrap_or_default(),
            inspect_url: l.inspect_url.clone(),
            steam_url: steam_url(&task.code, l),
            others_min,
            mid_profit: None,
            quick_profit: None,
            reason,
            detail: None,
        }
    }
}

async fn store_evaluations(h: &Hunt, rows: Vec<EvalRow>, now: i64) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    h.db.call(move |c| {
        let tx = c.transaction()?;
        for r in rows {
            tx.execute(
                "INSERT INTO evaluations(at, listing_id, name, price_cents, \"float\", seed, paint_index,
                    rare_group, stickers_json, keychains_json, inspect_url, steam_url, others_min_cents,
                    mid_profit_cents, quick_profit_cents, reason, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![
                    now, r.listing_id, r.name, r.price_cents, r.float, r.seed, r.paint_index, r.group,
                    r.stickers, r.keychains, r.inspect_url, r.steam_url, r.others_min, r.mid_profit,
                    r.quick_profit, r.reason, r.detail
                ],
            )?;
        }
        tx.execute(
            "DELETE FROM evaluations WHERE id <= (SELECT MAX(id) FROM evaluations) - ?1",
            [EVALUATIONS_KEEP],
        )?;
        tx.commit()
    })
    .await
}

fn no_estimate_reason(e: &sell::NoEstimate) -> &'static str {
    match e {
        sell::NoEstimate::NoData => "sem_historico",
        sell::NoEstimate::TooFewSales { .. } => "poucas_vendas",
        sell::NoEstimate::FewSaleDays { .. } => "poucos_dias_de_venda",
        sell::NoEstimate::Implausible => "implausivel",
        sell::NoEstimate::NoQuickPrice => "concorrente_mais_barato",
    }
}

/// O que faz o anúncio valer mais, para leigo; o tipo vem do prefixo da chave do grupo (`pricing::groups`).
fn why(
    name: &str,
    group: Option<&crate::pricing::groups::Group>,
    seed: Option<u16>,
    float: Option<f64>,
) -> String {
    if let Some(w) = seed.and_then(|s| crate::pricing::groups::named_why(name, s))
        && let Some(g) = group
    {
        return format!("{}: {w}", g.label);
    }
    let Some(g) = group else {
        return "Desconto: anunciado abaixo do preço normal desta skin; o seed e o float não pesam no preço."
            .into();
    };
    let seed = seed.map_or("?".into(), |s| s.to_string());
    let float = float.map_or("?".into(), |f| format!("{f:.4}").replace('.', ","));
    let label = &g.label;
    match g.key.split(':').next().unwrap_or("") {
        "cs" => format!(
            "Pattern de tabela ({label}): o seed {seed} põe a estampa numa posição das mais procuradas desta skin, e colecionador paga mais por ela."
        ),
        "bluegem" => format!(
            "{label}: o seed {seed} deixa a arma com bastante azul (Case Hardened), o que colecionador paga mais."
        ),
        "fade" if label.ends_with("99%+") => format!(
            "{label}: o seed {seed} dá um degradê quase completo, a versão mais procurada do Fade."
        ),
        "fade" => format!(
            "{label}: o seed {seed} tem esse % de degradê; as vendas do CSFloat de fades iguais ou melhores saíram acima do preço comum, mas não é Full Fade."
        ),
        "phase" => format!(
            "Fase {label}: a cor desta faca/arma Doppler ou Gamma é mais rara que as fases comuns."
        ),
        "learned" => format!(
            "{label}: o seed {seed} entrou porque as vendas do CSFloat dele (ou de seeds iguais ou piores) saíram bem acima do preço comum desta skin; confira a estampa antes de pagar mais."
        ),
        _ => format!("{label} (seed {seed}, float {float})."),
    }
}

/// Selo do raro: vendas fechadas do grupo (as do CSFloat guardadas e os anúncios da mesma skin no grupo que
/// sumiram em até 48 h sem relistagem, múltiplo = preço ÷ `base_cents`) e anúncios do grupo parados há mais de 48 h.
async fn seal_for(
    h: &Hunt,
    name: &str,
    grp: &str,
    seed: Option<u16>,
    price_cents: i64,
    base_cents: Option<i64>,
    now: i64,
) -> Seal {
    const FAST: i64 = super::teacher::FAST;
    let (name, grp) = (name.to_string(), grp.to_string());
    let r = h
        .db
        .call(move |c| {
            let mut sales: Vec<(Option<u16>, f64)> = c
                .prepare("SELECT seed, multiple FROM csfloat_sales WHERE grp = ?1")?
                .query_map([&grp], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            if let Some(b) = base_cents.filter(|&b| b > 0) {
                let gone: Vec<(Option<u16>, i64)> = c
                    .prepare(
                        "SELECT seed, price_cents FROM observations WHERE grp = ?1 AND name = ?2 AND relisted = 0
                         AND gone_at IS NOT NULL AND gone_at - first_seen <= ?3",
                    )?
                    .query_map(params![grp, name, FAST], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                sales.extend(gone.into_iter().map(|(s, p)| (s, p as f64 / b as f64)));
            }
            // ainda visto nas últimas 6 h: o registro não marca sumiço de consulta que deixou de rodar
            let parked: u32 = c.query_row(
                "SELECT count(*) FROM observations WHERE grp = ?1 AND gone_at IS NULL AND first_seen <= ?2
                 AND last_seen >= ?3",
                params![grp, now - FAST, now - 6 * 3600],
                |r| r.get(0),
            )?;
            Ok((sales, parked))
        })
        .await;
    let (sales, parked) = r.unwrap_or_else(|e| {
        tracing::warn!("selo do raro sem vendas fechadas: {e:#}");
        (vec![], 0)
    });
    premium::seal(
        price_cents,
        base_cents,
        &premium::premium(seed, &sales),
        parked,
    )
}

/// Uma remoção de chaveiro: pacote de 3 por US$ 0,99 na loja do jogo (fonte de terceiros, 11/2026).
const CHARM_DETACH_USD_CENTS: i64 = 33;

/// O que um chaveiro rende tirado da arma e vendido separado: preço público em reais, menos a taxa do
/// Mercado e a remoção.
fn charm_net(usd_cents: i64, ratio: f64) -> i64 {
    crate::pricing::fees::seller_gets(usd_to_brl(usd_cents, ratio))
        - usd_to_brl(CHARM_DETACH_USD_CENTS, ratio)
}

/// Soma do que os chaveiros do anúncio rendem (só os que venderam na última semana e rendem mais que a
/// remoção) e o texto que explica a conta. Sem razão US$→R$ medida, nada.
// shortcut: o preço é a média pública em US$ × razão, não o livro do chaveiro em R$; se um aviso de chaveiro
// não fechar na venda, ler o livro dele antes de avisar. A remoção sai na hora e devolve o chaveiro ao
// inventário (ajuda da Steam, "CS2 - Charms"); o chaveiro tirado guarda a trava de 7 dias da arma (informado
// pelo Erick em 11/11, fontes de terceiros), a mesma espera que `days_locked` já conta para a arma.
async fn charm_gain(h: &Hunt, l: &Listing, now: i64) -> Result<Option<(i64, String)>> {
    let Some((ratio, _)) = h.lock().ratio.filter(|_| !l.keychains.is_empty()) else {
        return Ok(None);
    };
    let names: Vec<String> = l.keychains.iter().map(|k| k.name.clone()).collect();
    let priced: Vec<(String, i64)> =
        h.db.call(move |c| {
            let mut out = vec![];
            for name in names {
                let usd: Option<i64> = c
                    .query_row(
                        "SELECT usd_cents FROM charm_prices WHERE name = ?1 AND observed_at >= ?2",
                        params![name, now - 7 * DAY],
                        |r| r.get(0),
                    )
                    .optional()?;
                out.extend(usd.map(|u| (name, u)));
            }
            Ok(out)
        })
        .await?;
    let (mut total, mut text) = (0, String::new());
    for (name, usd) in priced {
        let net = charm_net(usd, ratio);
        if net > 0 {
            total += net;
            text += &format!(
                " · {name} vende a {} (média pública, vendeu na semana) e rende {} tirado da arma, já com a taxa e a remoção; ele sai na hora, mas só vende depois dos 7 dias de trava, como a arma",
                brl(usd_to_brl(usd, ratio)),
                brl(net)
            );
        }
    }
    Ok((total > 0).then_some((total, text)))
}

/// Avalia um anúncio. `Ok` sem `found`: sem estimativa ou sem lucro (conclusivo). `Err`: falha de rede
/// ao buscar histórico — o anúncio não é marcado como visto e volta a ser avaliado na próxima consulta.
async fn evaluate_one(
    h: &Hunt,
    task: &Task,
    l: &Listing,
    others_min: Option<i64>,
    notify_min: i64,
) -> Result<Judged> {
    let now = h.now();
    let mut points = points_for(h, &l.name, now).await?;
    let hist_age = history_at(h, &l.name).await?.map_or(0, |t| now - t);
    let learned = super::patterns::group(&h.lock().learned, &l.name, l.seed);
    let group = group_of(&l.name, l.seed.map(u32::from), l.paint_index, l.float)
        .or_else(|| learned.as_ref().map(|(g, _)| g.clone()));
    let correction = h.correction(group.is_some());
    sell::discount(&mut points, correction);
    // raro: preço médio comum da mesma skin, sem o prêmio do grupo (base do múltiplo e do selo)
    let base_cents = group
        .as_ref()
        .and_then(|_| sell::estimate(&points, now, None, None).ok())
        .map(|e| e.mid.price_cents);
    let mut rare = match (&group, learned) {
        // pattern aprendido: o prêmio são as vendas que ensinaram (todos os desgastes), não as desta página
        (Some(g), Some((lg, multiples))) if g.key == lg.key => Some(RareEvidence { multiples }),
        (Some(g), _) => Some(rare_evidence(h, &l.name, &g.key).await?),
        (None, _) => None,
    };
    // Vende-se na Steam: com fechamentos suficientes lá, o prêmio é o que a Steam pagou, não o do CSFloat.
    let mut on_steam = false;
    if let (Some(g), Some(b), Some(r)) = (&group, base_cents.filter(|b| *b > 0), rare.as_mut()) {
        let closed = steam_closed(h, &l.name, &g.key, b).await?;
        if closed.len() >= sell::RARE_MIN_SALES {
            (r.multiples, on_steam) = (closed, true);
        }
    }
    let mut est = match sell::estimate(&points, now, others_min, rare) {
        Ok(est) => est,
        Err(e) => {
            return Ok(Judged {
                found: None,
                near: None,
                refused: Some(no_estimate_reason(&e)),
                detail: Some(format!("{e:?}")),
                ghost: None,
            });
        }
    };
    if on_steam {
        est.evidence +=
            " · prêmio medido na Steam: anúncios do grupo que sumiram em até 48 h sem voltar";
    }
    if hist_age >= HISTORY_TTL {
        est.evidence += &format!(
            " · histórico de preços de {} h atrás, a Steam não deixou atualizar",
            hist_age / 3600
        );
    }
    if correction < 1.0 {
        let teacher = group.is_none() && {
            let s = h.lock();
            s.refit < s.correction[0]
        };
        est.evidence += &if teacher {
            format!(
                " · o mercado pagou {}% abaixo do previsto nos avisos da última semana, já descontado",
                ((1.0 - correction) * 100.0).round()
            )
        } else {
            format!(
                " · suas vendas saíram {}% abaixo do previsto, já descontado",
                ((1.0 - correction) * 100.0).round()
            )
        };
    }
    // O chaveiro sai da arma e vende separado: o que ele rende abate do custo (no mínimo 1 centavo).
    let charm = charm_gain(h, l, now).await?;
    if let Some((_, text)) = &charm {
        est.evidence += text;
    }
    let gain = charm.as_ref().map_or(0, |c| c.0);
    let cost = (l.price_cents - gain).max(1);
    let mut ev = sell::evaluate(cost, &est, notify_min);
    // chaveiro que rende mais do que a arma custa: o que passou do custo entra direto no lucro
    let extra = gain - (l.price_cents - cost);
    for t in [&mut ev.sell.high, &mut ev.sell.mid, &mut ev.sell.quick] {
        t.profit_cents += extra;
    }
    if gain > 0 {
        // a margem é sobre o que sai do saldo, não sobre o custo abatido
        ev.margin_pct = ev.sell.mid.profit_cents as f64 * 100.0 / l.price_cents as f64;
    }
    let near = Some(Near {
        name: l.name.clone(),
        price_cents: l.price_cents,
        mid_cents: ev.sell.mid.profit_cents,
        quick_cents: ev.sell.quick.profit_cents,
    });
    // Com chaveiro o professor não aprende: o lucro não vem só da venda da arma.
    let ghost_worthy =
        charm.is_none() && super::teacher::ghost_worthy(l.price_cents, ev.sell.mid.profit_cents);
    let ghost = ghost_worthy.then(|| super::teacher::Ghost {
        listing_id: l.listing_id.clone(),
        name: l.name.clone(),
        grp: group.as_ref().map(|g| g.key.clone()),
        price_cents: l.price_cents,
        mid_price_cents: ev.sell.mid.price_cents,
        mid_net_cents: ev.sell.mid.net_cents,
        base_cents,
        approved: ev.listable,
    });
    if !ev.listable {
        return Ok(Judged {
            found: None,
            near,
            refused: Some("sem_lucro"),
            detail: Some(est.evidence),
            ghost,
        });
    }
    let seal = match &group {
        Some(g) => Some(Box::new(
            seal_for(h, &l.name, &g.key, l.seed, l.price_cents, base_cents, now).await,
        )),
        None => None,
    };
    // Raro sem nenhuma venda fechada que prove o prêmio só é oportunidade se, vendido como comum, perde no máximo
    // centavos (11/11: Serpent Strike #804 a R$ 3,56 com prêmio de 3 vendas no CSFloat, 0 fechadas, −R$ 0,91 no rápido;
    // 13/11: MAC-10 Snow Splash #309 a R$ 0,34, 20–56× no CSFloat, recusado por −R$ 0,18).
    if seal.as_ref().is_some_and(|s| !s.proven && s.sales == 0)
        && unproven_loses_too_much(ev.sell.quick.profit_cents)
    {
        return Ok(Judged {
            found: None,
            near,
            refused: Some("raro_nao_comprovado"),
            detail: Some(est.evidence),
            ghost,
        });
    }
    let opp = Opportunity {
        id: l.listing_id.clone(),
        name: l.name.clone(),
        icon_url: l.icon_url.clone(),
        steam_url: steam_url(&task.code, l),
        inspect_url: l.inspect_url.clone(),
        wear: wear_label(&l.name).map(str::to_string),
        float: l.float,
        seed: l.seed.map(i64::from),
        group: group.as_ref().map(|g| g.label.clone()),
        why: Some(why(&l.name, group.as_ref(), l.seed, l.float)),
        kind: if group.is_some() {
            Kind::Raro
        } else {
            Kind::Desconto
        },
        price_cents: l.price_cents,
        sell: api_sell(&ev.sell, correction),
        margin_pct: ev.margin_pct,
        safe: ev.safe,
        confidence: api_confidence(est.confidence),
        ev_profit_cents: ev.ev_profit_cents,
        days_locked: ev.days_locked,
        profit_per_day_cents: ev.profit_per_day_cents,
        risk: Some(match est.risk {
            sell::Risk::Low => Risk::Baixo,
            sell::Risk::Medium => Risk::Medio,
            sell::Risk::High => Risk::Alto,
        }),
        evidence: est.evidence,
        seen_at: now,
        checked_at: Some(now),
        gone_at: None,
        gone_reason: None,
        seal,
    };
    let detail = Some(opp.evidence.clone());
    Ok(Judged {
        found: Some((opp, ev.notify)),
        near,
        refused: None,
        detail,
        ghost,
    })
}

/// Processa uma página de uma tarefa. Ver o módulo para as regras.
pub async fn process(h: &Hunt, task: &Task, page: &ListingsPage) -> Result<Outcome> {
    let now = h.now();
    let saldo = h.saldo();
    let relevant: Vec<&Listing> = page
        .listings
        .iter()
        .filter(|l| task.rare() || l.name == task.name)
        .collect();
    let mut out = Outcome::default();
    let floor = relevant.iter().map(|l| l.price_cents).min();
    {
        let mut s = h.lock();
        let previous = s.floors.insert(task.floor_key(), (floor, now));
        if floor.is_some_and(|p| p > saldo) && previous.is_none_or(|(p, _)| p != floor) {
            // A seleção já sabe excluir o alvo caro; não precisa esperar os 15 min periódicos.
            s.selected = None;
        }
    }
    let learned = h.lock().learned.clone();
    let key = |x: &Listing| {
        group_of(&x.name, x.seed.map(u32::from), x.paint_index, x.float)
            .or_else(|| super::patterns::group(&learned, &x.name, x.seed).map(|(g, _)| g))
            .map(|g| g.key)
    };
    // Raro: o menor anúncio de cada (nome, grupo) nas outras faixas, vistas há menos de FLOOR_TTL.
    // shortcut: o concorrente novo de uma faixa só pesa na oportunidade de outra quando a dela é consultada de
    // novo; se isso deixar lucro velho na tela, reavaliar as ativas do mesmo grupo aqui.
    let mut elsewhere = super::Rivals::new();
    if task.rare() {
        let mut here = super::Rivals::new();
        for l in &relevant {
            let p = here
                .entry((l.name.clone(), key(l)))
                .or_insert(l.price_cents);
            *p = (*p).min(l.price_cents);
        }
        let (mine, prefix) = (task.floor_key(), format!("{}#", task.id));
        let mut s = h.lock();
        for (_, (_, m)) in s
            .rivals
            .iter()
            .filter(|(k, (at, _))| **k != mine && k.starts_with(&prefix) && now - at < FLOOR_TTL)
        {
            for (g, &p) in m {
                let e = elsewhere.entry(g.clone()).or_insert(p);
                *e = (*e).min(p);
            }
        }
        s.rivals.insert(mine, (now, here));
    }
    let ids: Vec<String> = relevant.iter().map(|l| l.listing_id.clone()).collect();
    if let Err(error) = h.db.call(move |c| {
        let tx = c.transaction()?;
        for id in ids {
            let is_new = tx.execute("INSERT OR IGNORE INTO activity_seen(listing_id, last_seen) VALUES (?1, ?2)", params![id, now])? == 1;
            if is_new {
                tx.execute("INSERT INTO hourly_activity(hour_utc, listings) VALUES (?1, 1) ON CONFLICT(hour_utc) DO UPDATE SET listings = listings + 1", [now.div_euclid(3600)])?;
            } else {
                tx.execute("UPDATE activity_seen SET last_seen = ?2 WHERE listing_id = ?1", params![id, now])?;
            }
        }
        tx.commit()
    }).await {
        tracing::warn!(%error, "falha ao registrar atividade de anúncios");
    }

    // --- sumiu? só com página confiável; fora da primeira página o anúncio pode só ter sido empurrado
    let trust = {
        let mut s = h.lock();
        if page.listings.is_empty() {
            let n = s.empty_streak.entry(task.id.clone()).or_default();
            *n = n.saturating_add(1);
            *n >= EMPTY_PAGES_TO_TRUST
        } else {
            s.empty_streak.remove(&task.id);
            true
        }
    };
    if trust {
        let present: HashSet<&str> = relevant.iter().map(|l| l.listing_id.as_str()).collect();
        let page_max = page
            .listings
            .iter()
            .map(|l| l.price_cents)
            .max()
            .unwrap_or(0);
        let missing: Vec<String> = h
            .lock()
            .active
            .values()
            .filter(|a| a.task == task.id && !present.contains(a.opp.id.as_str()))
            // raro em rodízio: a página só cobre a faixa de seed consultada
            .filter(|a| {
                task.seeds.first().is_none_or(|&(lo, hi)| {
                    a.opp
                        .seed
                        .is_some_and(|s| (i64::from(lo)..=i64::from(hi)).contains(&s))
                })
            })
            .filter(|a| !page.more || a.opp.price_cents <= page_max)
            .map(|a| a.opp.id.clone())
            .collect();
        out.gone = mark_gone(h, missing, GoneReason::Unavailable).await?;
    }

    // --- novos
    let ids: Vec<String> = relevant.iter().map(|l| l.listing_id.clone()).collect();
    let known: HashSet<String> =
        h.db.call(move |c| {
            let mut known = HashSet::new();
            for id in ids {
                let hit = c
                    .query_row(
                        "SELECT 1 FROM listings_seen WHERE listing_id = ?1",
                        [&id],
                        |_| Ok(()),
                    )
                    .optional()?;
                if hit.is_some() {
                    known.insert(id);
                }
            }
            Ok(known)
        })
        .await?;
    let view = h.settings.view().await?;
    let notify_min = view.notify_min_profit_cents;
    let mut seen: Vec<&Listing> = relevant
        .iter()
        .copied()
        .filter(|l| known.contains(&l.listing_id))
        .collect();
    let mut audit: Vec<EvalRow> = Vec::new();
    for l in relevant.iter().copied() {
        let was_known = known.contains(&l.listing_id);
        let active = h.lock().active.get(&l.listing_id).map(|a| a.opp.clone());
        if l.price_cents > saldo && active.is_none() {
            // Guarda uma vez por ciclo: o mesmo anúncio volta em toda consulta da tarefa.
            if h.lock().cycle.over_saldo.insert(l.listing_id.clone()) {
                audit.push(EvalRow::new(task, l, None, "acima_do_saldo"));
            }
            continue; // não cabe agora; se o saldo subir, ainda é novo
        }
        // Concorrência = outro anúncio do mesmo nome e do mesmo grupo raro (ou ambos comuns): um comum
        // barato não segura a venda de um T1. Inclui as outras faixas do mesmo raro.
        let mine = key(l);
        let others_min = page
            .listings
            .iter()
            .filter(|o| o.name == l.name && o.listing_id != l.listing_id && key(o) == mine)
            .map(|o| o.price_cents)
            .chain(elsewhere.get(&(l.name.clone(), mine.clone())).copied())
            .min();
        // Já visto: só reavalia se o preço, o histórico ou o menor concorrente mudaram desde a última
        // avaliação (ou se é oportunidade ativa e o histórico passou do prazo: a avaliação pode rebaixá-la).
        // shortcut: a janela de 7 dias anda sem mudar a chave; um dia que sai dela só pesa quando o histórico
        // vence (até 6 h). Se isso deixar oportunidade velha na tela, guardar o dia da janela na chave.
        let hist = history_at(h, &l.name).await?;
        if was_known {
            let last = h.lock().evaluated.get(&l.listing_id).copied();
            let aged = active.is_some() && hist.is_none_or(|t| now - t >= HISTORY_TTL);
            if last == Some((l.price_cents, hist.unwrap_or(0), others_min)) && !aged {
                if let Some(mut opp) = active {
                    let changed = opp.checked_at != Some(now);
                    opp.checked_at = Some(now);
                    let notify = should_notify(&opp, notify_min);
                    let (_, alert) =
                        store_opportunity(h, task, l, &opp, false, notify, view.panel_url.clone())
                            .await?;
                    if changed || alert {
                        out.found.push((opp, alert));
                    }
                }
                continue;
            }
        }
        let judged = match evaluate_one(h, task, l, others_min, notify_min).await {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("avaliação de {} adiada: {e:#}", l.listing_id);
                continue;
            }
        };
        let new_credit = !was_known && super::explore::first_credit(h, &l.listing_id).await?;
        let mut row = EvalRow::new(task, l, others_min, judged.refused.unwrap_or("aprovada"));
        row.mid_profit = judged.near.as_ref().map(|n| n.mid_cents);
        row.quick_profit = judged.near.as_ref().map(|n| n.quick_cents);
        row.detail = judged.detail.clone();
        audit.push(row);
        let hist_now = history_at(h, &l.name).await?.unwrap_or(0);
        {
            // Só anúncio novo ensina: reavaliar o mesmo (histórico mudou) não rende de novo.
            out.rewarded.fresh += u32::from(new_credit);
            if let Some(n) = judged.near.as_ref().filter(|_| judged.found.is_none()) {
                out.rewarded.near_hits += u32::from(new_credit && is_near(n));
            }
            let mut s = h.lock();
            if s.evaluated.len() >= EVALUATED_MAX {
                s.evaluated.clear();
            }
            s.evaluated
                .insert(l.listing_id.clone(), (l.price_cents, hist_now, others_min));
            if let Some(why) = judged.refused {
                s.cycle.refuse(why);
            }
            s.cycle.record(judged.found.is_some(), judged.near);
        }
        if !was_known {
            seen.push(l);
            if let Some(g) = judged.ghost {
                super::teacher::add_ghost(h, g, now).await;
            }
        }
        match (judged.found, active) {
            (Some((mut opp, notify)), prev) => {
                let fresh = prev.is_none();
                if let Some(p) = &prev {
                    opp.seen_at = p.seen_at; // a ordem da lista não muda por reavaliar
                }
                let (first_approved, alert) =
                    store_opportunity(h, task, l, &opp, fresh, notify, view.panel_url.clone())
                        .await?;
                if first_approved {
                    out.rewarded.finds += 1;
                    out.rewarded.gain_cents += opp.sell.mid.profit_cents;
                }
                out.found.push((opp, alert));
            }
            (None, Some(_)) => out
                .gone
                .extend(mark_gone(h, vec![l.listing_id.clone()], GoneReason::Unprofitable).await?),
            (None, None) => {}
        }
    }
    store_seen(h, &seen, now).await?;
    if let Err(error) = store_evaluations(h, audit, now).await {
        tracing::warn!(%error, "falha ao guardar avaliações para auditoria");
    }

    if !out.found.is_empty() || !out.gone.is_empty() {
        h.recompute_combo();
        h.publish_state();
    }
    Ok(out)
}

/// id, nome, preço, seed, float.
type SeenRow = (String, String, i64, Option<i64>, Option<f64>);

async fn store_seen(h: &Hunt, seen: &[&Listing], now: i64) -> Result<()> {
    let rows: Vec<SeenRow> = seen
        .iter()
        .map(|l| {
            (
                l.listing_id.clone(),
                l.name.clone(),
                l.price_cents,
                l.seed.map(i64::from),
                l.float,
            )
        })
        .collect();
    h.db.call(move |c| {
        let tx = c.transaction()?;
        for (id, name, price, seed, float) in rows {
            tx.execute(
                "INSERT INTO listings_seen(listing_id, name, price_cents, seed, \"float\", first_seen, last_seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                 ON CONFLICT(listing_id) DO UPDATE SET last_seen = excluded.last_seen,
                    price_cents = excluded.price_cents",
                params![id, name, price, seed, float, now],
            )?;
        }
        tx.commit()
    })
    .await
}

/// Guarda a oportunidade e o anúncio inteiro (a compra refaz o pedido a partir dele) e publica o evento.
async fn store_opportunity(
    h: &Hunt,
    task: &Task,
    l: &Listing,
    opp: &Opportunity,
    fresh: bool,
    notify: bool,
    panel_url: Option<String>,
) -> Result<(bool, bool)> {
    let (id, json, lj, tid) = (
        opp.id.clone(),
        serde_json::to_string(opp)?,
        serde_json::to_string(l)?,
        task.id.clone(),
    );
    let at = opp.seen_at;
    let (decision, now, profit) = (
        crate::research::current().map(|ctx| ctx.id),
        h.now(),
        opp.sell.mid.profit_cents,
    );
    let (first, alert, changed) = h.db.call(move |c| {
        let tx = c.transaction()?;
        let previous: Option<String> = tx.query_row(
            "SELECT json FROM opportunities WHERE id = ?1", [&id], |r| r.get(0)
        ).optional()?;
        let changed = previous.as_ref() != Some(&json);
        let first = fresh && super::explore::record_find(&tx,&id,decision,now,&tid,profit)?;
        if first
            && let Err(error) = tx.execute("INSERT INTO hourly_activity(hour_utc, opportunities) VALUES (?1, 1) ON CONFLICT(hour_utc) DO UPDATE SET opportunities = opportunities + 1", [at.div_euclid(3600)]) {
            tracing::warn!(%error, "falha ao registrar atividade de oportunidades");
        }
        tx.execute(
            "INSERT INTO opportunities(id, json, created_at, gone_at, task, listing_json)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET json=excluded.json, gone_at=NULL,
                 task=excluded.task, listing_json=excluded.listing_json",
            params![id, json, at, tid, lj],
        )?;
        // Reserva com prazo: se o processo fechar durante o envio, a próxima consulta tenta de novo.
        let alert = notify && tx.execute(
            "UPDATE opportunities SET notify_pending_at=?2 WHERE id=?1 AND notified_at IS NULL
             AND (notify_pending_at IS NULL OR notify_pending_at <= ?3)",
            params![id, now, now - 60],
        )? == 1;
        tx.commit()?;
        Ok((first, alert, changed))
    })
    .await?;
    {
        let mut s = h.lock();
        s.active.insert(
            opp.id.clone(),
            Active {
                opp: opp.clone(),
                task: task.id.clone(),
                attempted_at: now,
            },
        );
    }
    if changed {
        let _ = h.events.send(Event::Opportunity(opp.clone()));
    }
    if alert {
        send_alert(h, opp, panel_url, now);
    }
    Ok((first, alert))
}

/// Marca como encerradas (`gone_at`) e avisa o painel. Devolve os ids que estavam ativos.
pub(crate) async fn mark_gone(
    h: &Hunt,
    ids: Vec<String>,
    reason: GoneReason,
) -> Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(ids);
    }
    let now = h.now();
    let list = ids.clone();
    let reason_json = serde_json::to_string(&reason)?;
    h.db.call(move |c| {
        let tx = c.transaction()?;
        for id in &list {
            tx.execute(
                "UPDATE opportunities SET gone_at = ?2, json=json_set(json, '$.gone_reason', json(?3))
                 WHERE id = ?1 AND gone_at IS NULL",
                params![id, now, reason_json],
            )?;
        }
        tx.commit()
    })
    .await?;
    {
        let mut s = h.lock();
        for id in &ids {
            s.active.remove(id);
        }
    }
    for id in &ids {
        let _ = h.events.send(Event::Gone {
            id: id.clone(),
            at: now,
            reason,
        });
    }
    Ok(ids)
}

/// Tarefas que saíram da seleção deixam de ser conferidas: suas oportunidades não podem ficar na lista
/// para sempre como se ainda estivessem vivas.
pub async fn drop_tasks(h: &Hunt, tasks: &[String]) -> Result<()> {
    let ids: Vec<String> = h
        .lock()
        .active
        .values()
        .filter(|a| tasks.contains(&a.task))
        .map(|a| a.opp.id.clone())
        .collect();
    if !mark_gone(h, ids, GoneReason::Unwatched).await?.is_empty() {
        h.recompute_combo();
    }
    Ok(())
}

/// Confirma o envio no banco; falha ou reinício permite repetir na próxima conferência do anúncio.
/// Em segundo plano: o Windows e o Telegram não podem segurar a caça.
fn send_alert(h: &Hunt, opp: &Opportunity, panel_url: Option<String>, reserved_at: i64) {
    let title = format!("{} de lucro · {}", brl(opp.sell.mid.profit_cents), opp.name);
    let body = format!(
        "Compre por {} e venda por {}.{} Clique para abrir na Steam.",
        brl(opp.price_cents),
        brl(opp.sell.mid.price_cents),
        opp.seal
            .as_ref()
            .map(|s| format!(" Raro {}.", s.text))
            .unwrap_or_default()
    );
    let url = opp.steam_url.clone();
    // raro: o selo e, se o prêmio dá folga, o preço para anunciar
    let seal = opp
        .seal
        .as_ref()
        .map(|s| match (s.open_cents, s.floor_cents) {
            (Some(open), Some(floor)) if open > floor => {
                format!(
                    "Raro {} · anuncie a {}, desça até {}",
                    s.text,
                    brl(open),
                    brl(floor)
                )
            }
            _ => format!("Raro {}", s.text),
        });
    let tg = h.telegram.clone();
    let db = h.db.clone();
    let now = h.now();
    let alert = Alert {
        id: opp.id.clone(),
        name: opp.name.clone(),
        group: opp.group.clone(),
        price_cents: opp.price_cents,
        mid_profit_cents: opp.sell.mid.profit_cents,
        safe_profit_cents: opp.sell.quick.profit_cents,
        eta_hours: opp.sell.mid.eta_hours,
        seal,
    };
    tokio::spawn(async move {
        let telegram = async {
            let Some(tg) = tg else {
                return false;
            };
            for attempt in 0..3 {
                match tg.notify(&alert, panel_url.as_deref()).await {
                    Ok(_) => return true,
                    Err(NotifyError::NotConfigured) => return false,
                    Err(NotifyError::Transport(e)) if attempt < 2 => {
                        tracing::warn!("aviso no Telegram adiado: {e}");
                        tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
                    }
                    Err(e) => {
                        tracing::warn!("aviso no Telegram falhou: {e}");
                        return false;
                    }
                }
            }
            false
        };
        // Inclui espera pelo mutex e pelo ritmo: o envio inteiro termina antes da reserva de 60 s.
        let telegram = async {
            tokio::time::timeout(std::time::Duration::from_secs(40), telegram)
                .await
                .unwrap_or_else(|_| {
                    tracing::warn!("o Telegram não confirmou o aviso em 40 s");
                    false
                })
        };
        let (desktop, telegram) =
            tokio::join!(crate::notify::desktop(&title, &body, &url), telegram);
        let desktop = desktop.unwrap_or_else(|e| {
            tracing::warn!("aviso do Windows falhou: {e}");
            false
        });
        let delivered = desktop || telegram;
        if delivered {
            tracing::info!(id = %alert.id, windows = desktop, telegram, "aviso entregue");
        }
        if let Err(e) = db
            .call(move |c| {
                c.execute(
            "UPDATE opportunities SET notify_pending_at=NULL, notified_at=COALESCE(notified_at,?2)
             WHERE id=?1 AND notify_pending_at=?3",
            params![alert.id, delivered.then_some(now), reserved_at],
        )
            })
            .await
        {
            tracing::warn!("não consegui registrar o envio do aviso: {e:#}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::rare_multiples;

    #[test]
    fn why_names_the_reason_for_each_kind() {
        use crate::pricing::groups::Group;
        let g = |key: &str, label: &str| Group {
            key: key.into(),
            label: label.into(),
        };
        assert!(super::why("", None, Some(1), Some(0.2)).starts_with("Desconto"));
        let learned = g(
            "learned:Desert Eagle | Serpent Strike:804",
            "Max Orange #804",
        );
        let w = super::why("", Some(&learned), Some(804), Some(0.21));
        assert!(
            w.starts_with("Max Orange #804: o seed 804") && w.contains("CSFloat"),
            "{w}"
        );
        let cs = g("cs:Glock-18 | Moonrise:t1", "T1 Center Star");
        assert!(
            super::why("", Some(&cs), Some(694), None)
                .contains("Pattern de tabela (T1 Center Star)")
        );
    }

    #[test]
    fn rare_premium_uses_the_whole_group_when_the_name_has_few_sales() {
        let row = |name: &str, float: f64, multiple: f64, grp: Option<&str>| {
            (
                name.to_string(),
                Some(float),
                multiple,
                grp.map(str::to_string),
            )
        };
        let (ft, mw) = ("Skin (Field-Tested)", "Skin (Minimal Wear)");
        // venda sem grupo (float baixo ou não) não entra em prêmio nenhum
        let mut rows = vec![row(ft, 0.15, 3.0, None), row(mw, 0.07, 9.0, None)];
        assert!(rare_multiples(&rows, ft, "float:Skin:Field-Tested").is_empty());
        // tabela: 2 vendas no nome são poucas, valem as 3 do grupo em todos os desgastes
        rows.push(row(ft, 0.3, 1.5, Some("cs:Skin:t1")));
        rows.push(row(ft, 0.3, 1.7, Some("cs:Skin:t1")));
        rows.push(row(mw, 0.1, 2.0, Some("cs:Skin:t1")));
        rows.push(row(mw, 0.1, 5.0, Some("cs:Skin:t2")));
        assert_eq!(rare_multiples(&rows, ft, "cs:Skin:t1"), vec![1.5, 1.7, 2.0]);
    }
}
