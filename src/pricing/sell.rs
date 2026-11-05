//! Três preços de venda, prazo e confiança a partir do histórico da Steam (docs/projeto.md, "Precificação").
//!
//! Tudo aqui é puro: recebe pontos e múltiplos já convertidos, devolve números. Todo preço é o que o
//! COMPRADOR paga (como na tabela da Steam); o líquido do vendedor sai de `fees::seller_gets`.
//! A skin comprada fica 7 dias travada, então os preços valem para daqui a 7 dias e o prazo
//! (`eta_hours`) conta a partir do fim da trava.

use super::fees::seller_gets;
use std::fmt;

const DAY: i64 = 86_400;
/// Janela da base: últimos 7 dias.
const BASE_DAYS: i64 = 7;
/// Tendência: últimos 3 dias contra os dias 4–14.
const TREND_RECENT_DAYS: i64 = 3;
const TREND_OLD_DAYS: i64 = 14;
/// Distância entre o centro da janela recente (1,5 d atrás) e a antiga (9 d atrás).
const TREND_GAP_DAYS: f64 = 7.5;
/// Quanto à frente a queda é projetada.
const TREND_HORIZON_DAYS: f64 = 7.0;
const TREND_FLOOR: f64 = 0.85;
/// Cada janela da tendência precisa de ao menos isto em compras; abaixo, ruído → fator 1.
const TREND_MIN_PURCHASES: u32 = 5;
/// Menos que isto em 7 dias não sustenta preço nenhum.
const MIN_PURCHASES: u32 = 3;
/// Vendas concentradas em menos dias que isto (um pico ou um lote velho) não sustentam o preço de hoje.
const MIN_SALE_DAYS: usize = 3;

/// Menor preço que rende algo ao vendedor: `buyer_pays(1)` = 1 + 1 + 1 centavos.
pub const MIN_PRICE_CENTS: i64 = 3;
/// Teto sensato para um preço de skin (R$ 100.000); acima disso o dado é lixo.
pub const MAX_PRICE_CENTS: i64 = 10_000_000;
/// Múltiplo raro acima disto é dado ruim (venda com adesivo/erro), não prêmio de pattern.
pub const MAX_RARE_MULTIPLE: f64 = 100.0;

pub const MIN_LIST_PROFIT_CENTS: i64 = 20;
pub const MIN_LIST_MARGIN_PCT: f64 = 5.0;

/// Ponto do histórico da Steam: mediana paga naquele balde e quantas compras entraram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PricePoint {
    pub ts: i64,
    pub median_cents: i64,
    pub purchases: u32,
}

/// Vendas pagas do mesmo grupo raro (CSFloat): cada `price / reference.base_price`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RareEvidence {
    pub multiples: Vec<f64>,
}

/// Mínimo de vendas do grupo para o prêmio virar preço.
pub const RARE_MIN_SALES: usize = 3;
const RARE_HIGH_CONFIDENCE_SALES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    /// Texto do contrato da API.
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::High => "alta",
            Confidence::Medium => "media",
            Confidence::Low => "baixa",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EstTier {
    pub price_cents: i64,
    /// Horas esperadas até vender a esse preço, contadas depois da trava. `None` = sem medida.
    pub eta_hours: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SellEstimate {
    pub quick: EstTier,
    pub mid: EstTier,
    pub high: EstTier,
    pub confidence: Confidence,
    pub probabilities: [f64; 3],
    pub volatility: Option<f64>,
    pub risk: Risk,
    /// Frase curta em português para leigo.
    pub evidence: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    Low,
    Medium,
    High,
}

fn daily_volatility(points: &[PricePoint], now: i64) -> Option<f64> {
    let mut days = std::collections::BTreeMap::<i64, Vec<(f64, f64)>>::new();
    for p in points
        .iter()
        .filter(|p| p.ts > now - 30 * DAY && p.ts <= now && p.purchases > 0 && p.median_cents > 0)
    {
        days.entry(p.ts.div_euclid(DAY))
            .or_default()
            .push((p.median_cents as f64, f64::from(p.purchases)));
    }
    let medians: Vec<_> = days
        .iter()
        .filter_map(|(day, values)| weighted_quantile(values, 0.5).map(|v| (*day, v)))
        .collect();
    let returns: Vec<_> = medians
        .windows(2)
        .filter(|w| w[1].0 == w[0].0 + 1)
        .map(|w| w[1].1 / w[0].1 - 1.0)
        .collect();
    if returns.len() < 2 {
        return None;
    }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    Some((returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64).sqrt())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoEstimate {
    /// Nenhum ponto válido nos últimos 7 dias.
    NoData,
    /// Há pontos, mas poucas compras para confiar.
    TooFewSales { purchases: u32 },
    /// As compras caíram em poucos dias distintos da janela.
    FewSaleDays { days: usize },
    /// Preço fora de uma faixa plausível (dado ruim no histórico ou no prêmio).
    Implausible,
    /// O menor outro anúncio é tão barato que nenhum preço rápido o bate e ainda rende algo.
    NoQuickPrice,
}

impl fmt::Display for NoEstimate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NoEstimate::NoData => write!(f, "sem vendas nos últimos 7 dias"),
            NoEstimate::Implausible => write!(f, "preço fora do plausível, dado ruim"),
            NoEstimate::NoQuickPrice => {
                write!(
                    f,
                    "o menor anúncio é barato demais para bater numa venda rápida"
                )
            }
            NoEstimate::FewSaleDays { days } => {
                write!(
                    f,
                    "vendas em só {days} dos últimos 7 dias (mínimo {MIN_SALE_DAYS})"
                )
            }
            NoEstimate::TooFewSales { purchases } => {
                write!(
                    f,
                    "só {purchases} vendas nos últimos 7 dias (mínimo {MIN_PURCHASES})"
                )
            }
        }
    }
}

/// Multiplica o histórico pelo fator das vendas reais (`< 1` desconta; `1` deixa intacto).
pub fn discount(points: &mut [PricePoint], factor: f64) {
    if factor < 1.0 {
        for p in points {
            p.median_cents = (p.median_cents as f64 * factor).floor() as i64;
        }
    }
}

/// Quantil ponderado, **sem interpolação**. Ordena os pares `(valor, peso)` por valor; o quantil `q` é o
/// primeiro valor cujo peso acumulado chega a `q × peso_total`. Com pesos inteiros isso é o mesmo que
/// "peso acumulado ≥ ceil(q × total)" de docs/conhecimento.md 5.2. Devolve sempre um valor observado.
/// Descarta valores não finitos e pesos ≤ 0; vazio → `None`. `q` fica em [0, 1].
///
/// Atenção (fraqueza 1 do 5.2): com poucos pontos de peso parecido, Q25 vira o mínimo e Q75 o máximo.
pub fn weighted_quantile(pairs: &[(f64, f64)], q: f64) -> Option<f64> {
    let mut v: Vec<(f64, f64)> = pairs
        .iter()
        .copied()
        .filter(|(x, w)| x.is_finite() && *w > 0.0)
        .collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: f64 = v.iter().map(|p| p.1).sum();
    let target = q.clamp(0.0, 1.0) * total;
    let mut cum = 0.0;
    for &(x, w) in &v {
        cum += w;
        if cum + 1e-9 >= target {
            return Some(x);
        }
    }
    v.last().map(|p| p.0)
}

fn pairs_in(points: &[PricePoint], after: i64, upto: i64) -> Vec<(f64, f64)> {
    points
        .iter()
        .filter(|p| p.ts > after && p.ts <= upto && p.purchases > 0 && p.median_cents > 0)
        .map(|p| (p.median_cents as f64, f64::from(p.purchases)))
        .collect()
}

fn weight(pairs: &[(f64, f64)]) -> f64 {
    pairs.iter().map(|p| p.1).sum()
}

/// Fator de tendência só de queda: mediana dos últimos 3 dias contra a dos dias 4–14, a razão projetada
/// em 7 dias (`r^(7/7,5)`, a distância entre os centros das janelas), piso 0,85. Alta não credita.
fn trend_factor(points: &[PricePoint], now: i64) -> f64 {
    let recent = pairs_in(points, now - TREND_RECENT_DAYS * DAY, now);
    let old = pairs_in(
        points,
        now - TREND_OLD_DAYS * DAY,
        now - TREND_RECENT_DAYS * DAY,
    );
    if weight(&recent) < f64::from(TREND_MIN_PURCHASES)
        || weight(&old) < f64::from(TREND_MIN_PURCHASES)
    {
        return 1.0;
    }
    let (Some(m3), Some(m14)) = (
        weighted_quantile(&recent, 0.5),
        weighted_quantile(&old, 0.5),
    ) else {
        return 1.0;
    };
    let r = m3 / m14;
    if r >= 1.0 {
        return 1.0;
    }
    r.powf(TREND_HORIZON_DAYS / TREND_GAP_DAYS).max(TREND_FLOOR)
}

/// 24 / (vendas por dia × fração do peso com mediana ≥ preço). `None` se ninguém pagou isso.
fn eta_hours(pairs: &[(f64, f64)], sales_per_day: f64, price: i64) -> Option<f64> {
    let total = weight(pairs);
    let at_or_above: f64 = pairs
        .iter()
        .filter(|p| p.0 >= price as f64)
        .map(|p| p.1)
        .sum();
    let rate = sales_per_day * at_or_above / total;
    (rate > 0.0).then(|| 24.0 / rate)
}

/// Número em português: vírgula decimal.
fn num(x: f64, decimals: usize) -> String {
    format!("{x:.decimals$}").replace('.', ",")
}

fn sales_word(n: usize) -> String {
    if n == 1 {
        "1 venda".into()
    } else {
        format!("{n} vendas")
    }
}

/// Estima os três preços de venda (para daqui a 7 dias).
///
/// - `points`: histórico (pode ter mais de 7 dias; a tendência usa até 14).
/// - `lowest_other_ask_cents`: menor anúncio OUTRO que o do item avaliado (limita o `quick` e o `mid`).
/// - `rare`: vendas do grupo raro; `None` = item comum. Com `n < 3` o prêmio não vira preço.
///
/// Vendas por dia = compras da janela ÷ 7 (conservador: dia sem venda conta). Em item raro com prêmio
/// provado o prazo fica `None`: o ritmo medido é o do item comum, não o do grupo.
pub fn estimate(
    points: &[PricePoint],
    now: i64,
    lowest_other_ask_cents: Option<i64>,
    rare: Option<RareEvidence>,
) -> Result<SellEstimate, NoEstimate> {
    let window = pairs_in(points, now - BASE_DAYS * DAY, now);
    if window.is_empty() {
        return Err(NoEstimate::NoData);
    }
    let purchases = weight(&window) as u32;
    if purchases < MIN_PURCHASES {
        return Err(NoEstimate::TooFewSales { purchases });
    }
    let days = points
        .iter()
        .filter(|p| p.ts > now - BASE_DAYS * DAY && p.ts <= now)
        .filter(|p| p.purchases > 0 && p.median_cents > 0)
        .map(|p| p.ts.div_euclid(DAY))
        .collect::<std::collections::HashSet<_>>()
        .len();
    if days < MIN_SALE_DAYS {
        return Err(NoEstimate::FewSaleDays { days });
    }
    let sales_per_day = f64::from(purchases) / BASE_DAYS as f64;
    let factor = trend_factor(points, now);
    let q = |p: f64| weighted_quantile(&window, p).unwrap_or(0.0) * factor;
    let (mut p25, mut p50, mut p75) = (q(0.25), q(0.5), q(0.75));

    let mut confidence = if sales_per_day >= 20.0 && window.len() >= 100 {
        Confidence::High
    } else if sales_per_day >= 5.0 {
        Confidence::Medium
    } else {
        Confidence::Low
    };

    let mut evidence = format!(
        "{} vendas/dia nos últimos 7 dias",
        if sales_per_day >= 10.0 {
            num(sales_per_day, 0)
        } else {
            num(sales_per_day, 1)
        }
    );
    if factor < 1.0 {
        evidence += &format!(
            " · preço em queda, descontado {}%",
            ((1.0 - factor) * 100.0).round()
        );
    }

    let mut proven_rare = false;
    let mut rare_samples = Vec::new();
    if let Some(r) = &rare {
        let m: Vec<(f64, f64)> = r
            .multiples
            .iter()
            .filter(|x| x.is_finite() && **x > 0.0 && **x <= MAX_RARE_MULTIPLE)
            .map(|&x| (x, 1.0))
            .collect();
        rare_samples = m.iter().map(|p| p.0).collect();
        if m.len() >= RARE_MIN_SALES {
            let (m25, m50, m75) = (
                weighted_quantile(&m, 0.25).unwrap_or(1.0),
                weighted_quantile(&m, 0.5).unwrap_or(1.0),
                weighted_quantile(&m, 0.75).unwrap_or(1.0),
            );
            // A venda rápida não conta com o prêmio: ele vem do CSFloat e já saiu bem abaixo na Steam
            // (previsto 2,33×, vendido 0,96×). Assim `safe` só marca o que paga mesmo sem prêmio.
            (p25, p50, p75) = (p25 * m25.min(1.0), p50 * m50, p75 * m75);
            proven_rare = true;
            evidence += &format!(
                " · grupo raro: {}, {}x a {}x o preço comum",
                sales_word(m.len()),
                num(m25, 1),
                num(m75, 1)
            );
            // prêmio com poucas vendas não sustenta confiança alta
            let cap = if m.len() >= RARE_HIGH_CONFIDENCE_SALES {
                Confidence::High
            } else {
                Confidence::Medium
            };
            confidence = confidence_min(confidence, cap);
        } else {
            evidence += &format!(" · prêmio sem prova ({})", sales_word(m.len()));
        }
    }

    if ![p25, p50, p75]
        .iter()
        .all(|x| x.is_finite() && *x <= MAX_PRICE_CENTS as f64)
    {
        return Err(NoEstimate::Implausible);
    }
    let price = |x: f64| (x.floor() as i64).max(MIN_PRICE_CENTS);
    let (mut quick, mut mid, high) = (price(p25), price(p50), price(p75));
    if let Some(ask) = lowest_other_ask_cents {
        let cap = ask.saturating_sub(1);
        if cap < MIN_PRICE_CENTS {
            return Err(NoEstimate::NoQuickPrice);
        }
        if cap < quick {
            quick = cap;
            evidence += " · venda rápida limitada pelo menor anúncio";
        }
        // O preço provável também: quem compra leva o anúncio mais barato, então vender acima do concorrente
        // que já está à venda é aposta, não o mais provável. O alto continua otimista.
        if cap < mid {
            mid = cap;
            evidence += " · preço médio limitado pelo menor anúncio";
        }
    }
    let tier = |p: i64| EstTier {
        price_cents: p,
        eta_hours: if proven_rare {
            None
        } else {
            eta_hours(&window, sales_per_day, p)
        },
    };
    let probability = |price: i64| {
        let multiples: &[f64] = if proven_rare { &rare_samples } else { &[1.0] };
        window
            .iter()
            .map(|(value, w)| {
                w * multiples
                    .iter()
                    .filter(|m| value * factor * **m >= price as f64)
                    .count() as f64
                    / multiples.len() as f64
            })
            .sum::<f64>()
            / weight(&window)
    };
    let volatility = daily_volatility(points, now);
    let risk = match volatility {
        Some(v)
            if v <= 0.02
                && sales_per_day >= 20.0
                && (rare.is_none() || rare_samples.len() >= 8) =>
        {
            Risk::Low
        }
        Some(v)
            if v <= 0.05 && sales_per_day >= 5.0 && (rare.is_none() || rare_samples.len() >= 3) =>
        {
            Risk::Medium
        }
        _ => Risk::High,
    };
    Ok(SellEstimate {
        probabilities: [probability(quick), probability(mid), probability(high)],
        volatility,
        risk,
        quick: tier(quick),
        mid: tier(mid),
        high: tier(high),
        confidence,
        evidence,
    })
}

fn confidence_min(a: Confidence, b: Confidence) -> Confidence {
    let rank = |c| match c {
        Confidence::High => 2,
        Confidence::Medium => 1,
        Confidence::Low => 0,
    };
    if rank(a) <= rank(b) { a } else { b }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SellTier {
    pub price_cents: i64,
    pub net_cents: i64,
    pub profit_cents: i64,
    pub eta_hours: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sell {
    pub high: SellTier,
    pub mid: SellTier,
    pub quick: SellTier,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Evaluation {
    pub sell: Sell,
    pub ev_profit_cents: Option<i64>,
    pub days_locked: Option<f64>,
    pub profit_per_day_cents: Option<i64>,
    /// Lucro no preço médio ÷ preço de compra, em %.
    pub margin_pct: f64,
    /// Lucro > 0 mesmo no preço rápido.
    pub safe: bool,
    /// Entra na lista: lucro no `mid` ≥ R$ 0,20 e margem ≥ 5 %.
    pub listable: bool,
    /// Vale avisar: entra na lista, tem lucro esperado positivo e `mid` chega ao mínimo. Não exige `safe`:
    /// o lucro esperado já pesa o risco, e raro quase nunca é `safe` (a venda rápida ignora o prêmio).
    pub notify: bool,
}

/// Avalia um anúncio de `price_cents` (total que o comprador paga) contra a estimativa.
pub fn evaluate(price_cents: i64, est: &SellEstimate, notify_min_cents: i64) -> Evaluation {
    let tier = |t: &EstTier| {
        let net = seller_gets(t.price_cents);
        SellTier {
            price_cents: t.price_cents,
            net_cents: net,
            profit_cents: net.saturating_sub(price_cents),
            eta_hours: t.eta_hours,
        }
    };
    let sell = Sell {
        high: tier(&est.high),
        mid: tier(&est.mid),
        quick: tier(&est.quick),
    };
    let margin_pct = if price_cents > 0 {
        sell.mid.profit_cents as f64 * 100.0 / price_cents as f64
    } else {
        0.0
    };
    // preço de compra ≤ 0 é entrada inválida: nada é seguro nem listável
    let valid = price_cents > 0;
    let safe = valid && sell.quick.profit_cents > 0;
    let listable = valid
        && sell.mid.profit_cents >= MIN_LIST_PROFIT_CENTS
        && margin_pct >= MIN_LIST_MARGIN_PCT;
    let total: f64 = est.probabilities.iter().sum();
    let ev_profit_cents = est.volatility.filter(|_| total > 0.0).map(|vol| {
        let expected = [
            sell.quick.profit_cents,
            sell.mid.profit_cents,
            sell.high.profit_cents,
        ]
        .iter()
        .zip(est.probabilities)
        .map(|(profit, p)| *profit as f64 * p / total)
        .sum::<f64>();
        (expected - price_cents as f64 * (vol * 7.0_f64.sqrt()).min(1.0)).round() as i64
    });
    let notify = listable
        && sell.mid.profit_cents >= notify_min_cents
        && ev_profit_cents.is_some_and(|profit| profit > 0);
    let days_locked = sell.mid.eta_hours.map(|hours| 7.0 + hours / 24.0);
    let profit_per_day_cents = ev_profit_cents
        .zip(days_locked)
        .map(|(profit, days)| (profit as f64 / days).round() as i64);
    Evaluation {
        ev_profit_cents,
        days_locked,
        profit_per_day_cents,
        sell,
        margin_pct,
        safe,
        listable,
        notify,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    /// Um ponto por hora, de `from_h` a `to_h` horas atrás, com `f(h)` = (mediana, compras).
    fn hourly(from_h: i64, to_h: i64, f: impl Fn(i64) -> (i64, u32)) -> Vec<PricePoint> {
        (to_h..from_h)
            .map(|h| {
                let (m, n) = f(h);
                PricePoint {
                    ts: NOW - h * 3600,
                    median_cents: m,
                    purchases: n,
                }
            })
            .collect()
    }

    fn flat(median: i64, per_hour: u32) -> Vec<PricePoint> {
        hourly(24 * 7, 0, |_| (median, per_hour))
    }

    fn rare(m: &[f64]) -> RareEvidence {
        RareEvidence {
            multiples: m.to_vec(),
        }
    }

    #[test]
    fn quantile_definition() {
        let p = [(10.0, 1.0), (20.0, 1.0), (30.0, 1.0), (40.0, 1.0)];
        assert_eq!(weighted_quantile(&p, 0.25), Some(10.0)); // acumulado 1 ≥ 1
        assert_eq!(weighted_quantile(&p, 0.5), Some(20.0)); // 2 ≥ 2
        assert_eq!(weighted_quantile(&p, 0.75), Some(30.0));
        assert_eq!(weighted_quantile(&p, 1.0), Some(40.0));
        assert_eq!(weighted_quantile(&p, 0.0), Some(10.0));
        // peso manda: 90 % do peso em 20
        let w = [(30.0, 1.0), (20.0, 9.0)];
        assert_eq!(weighted_quantile(&w, 0.25), Some(20.0));
        assert_eq!(weighted_quantile(&w, 0.95), Some(30.0));
        assert_eq!(weighted_quantile(&[], 0.5), None);
        assert_eq!(weighted_quantile(&[(5.0, 0.0), (f64::NAN, 1.0)], 0.5), None);
        assert_eq!(weighted_quantile(&[(5.0, 3.0)], 0.9), Some(5.0));
    }

    #[test]
    fn no_data() {
        assert_eq!(estimate(&[], NOW, None, None), Err(NoEstimate::NoData));
        // tudo fora da janela de 7 dias (e um ponto no futuro)
        let old = [
            PricePoint {
                ts: NOW - 8 * DAY,
                median_cents: 500,
                purchases: 50,
            },
            PricePoint {
                ts: NOW + 10,
                median_cents: 500,
                purchases: 50,
            },
            PricePoint {
                ts: NOW - 100,
                median_cents: 0,
                purchases: 50,
            },
            PricePoint {
                ts: NOW - 100,
                median_cents: 500,
                purchases: 0,
            },
        ];
        assert_eq!(estimate(&old, NOW, None, None), Err(NoEstimate::NoData));
        assert!(NoEstimate::NoData.to_string().contains("sem vendas"));
    }

    #[test]
    fn too_few_sales_and_single_point() {
        let one = [PricePoint {
            ts: NOW - 3600,
            median_cents: 500,
            purchases: 1,
        }];
        assert_eq!(
            estimate(&one, NOW, None, None),
            Err(NoEstimate::TooFewSales { purchases: 1 })
        );
        assert!(
            estimate(&one, NOW, None, None)
                .unwrap_err()
                .to_string()
                .contains("só 1")
        );
        // compras suficientes num dia só (pico ou lote velho): não sustenta preço
        let one_day = [PricePoint {
            ts: NOW - 3600,
            median_cents: 500,
            purchases: 10,
        }];
        assert_eq!(
            estimate(&one_day, NOW, None, None),
            Err(NoEstimate::FewSaleDays { days: 1 })
        );
        assert!(
            NoEstimate::FewSaleDays { days: 2 }
                .to_string()
                .contains("só 2 dos últimos 7 dias")
        );
        // três pontos em três dias: estima, confiança baixa, tudo igual
        let ok = [0, 1, 2].map(|d| PricePoint {
            ts: NOW - 3600 - d * DAY,
            median_cents: 500,
            purchases: 4,
        });
        let e = estimate(&ok, NOW, None, None).unwrap();
        assert_eq!(
            (e.quick.price_cents, e.mid.price_cents, e.high.price_cents),
            (500, 500, 500)
        );
        assert_eq!(e.confidence, Confidence::Low);
    }

    #[test]
    fn all_equal_is_flat_and_confident() {
        let e = estimate(&flat(663, 16), NOW, None, None).unwrap(); // 168 pontos, 384 vendas/dia
        assert_eq!(
            (e.quick.price_cents, e.mid.price_cents, e.high.price_cents),
            (663, 663, 663)
        );
        assert_eq!(e.confidence, Confidence::High);
        assert!(e.evidence.starts_with("384 vendas/dia"), "{}", e.evidence);
        // todo mundo pagou ≥ 663: 24 / 384 h
        assert!((e.mid.eta_hours.unwrap() - 24.0 / 384.0).abs() < 1e-9);
    }

    #[test]
    fn confidence_thresholds() {
        // 19 vendas/dia: média mesmo com muitos pontos; 133 vendas/7d → 19/dia
        let e = estimate(
            &hourly(24 * 7, 0, |h| (500, if h < 133 { 1 } else { 0 })),
            NOW,
            None,
            None,
        )
        .unwrap();
        assert_eq!(e.confidence, Confidence::Medium);
        // 20/dia mas poucos pontos (30 pontos com 5 compras): média
        let every = |n: i64, buys: u32| {
            hourly(24 * 7, 0, move |h| (500, if h % n == 0 { buys } else { 0 }))
        };
        let e = estimate(&every(5, 5), NOW, None, None).unwrap(); // 34 pontos
        assert_eq!(e.confidence, Confidence::Medium);
        // 4/dia: baixa
        let e = estimate(&every(6, 1), NOW, None, None).unwrap();
        assert_eq!(e.confidence, Confidence::Low);
        assert_eq!(e.confidence.as_str(), "baixa");
    }

    #[test]
    fn quantiles_use_purchase_weights() {
        // 1 compra a 1000 contra 99 a 500: Q75 continua 500
        let pts = [
            PricePoint {
                ts: NOW - 3600,
                median_cents: 1000,
                purchases: 1,
            },
            PricePoint {
                ts: NOW - 7200 - DAY,
                median_cents: 500,
                purchases: 50,
            },
            PricePoint {
                ts: NOW - 7200 - 2 * DAY,
                median_cents: 500,
                purchases: 49,
            },
        ];
        let e = estimate(&pts, NOW, None, None).unwrap();
        assert_eq!(e.high.price_cents, 500);
    }

    #[test]
    fn strong_drop_hits_the_floor() {
        // dias 4–14 a 1000, últimos 3 dias a 600 (r = 0,6 → piso 0,85); a janela de 7 dias é metade-metade
        let pts = hourly(24 * 14, 0, |h| (if h < 72 { 600 } else { 1000 }, 4));
        let e = estimate(&pts, NOW, None, None).unwrap();
        // Q50 da janela de 7 dias: pesos 72 h × 600 contra 96 h × 1000 → 1000; × 0,85
        assert_eq!(e.mid.price_cents, 850);
        assert_eq!(e.quick.price_cents, 510); // Q25 = 600 × 0,85
        assert!(
            e.evidence.contains("queda, descontado 15%"),
            "{}",
            e.evidence
        );
    }

    #[test]
    fn mild_drop_is_projected_not_floored() {
        // r = 0,95 → f = 0,95^(7/7,5) ≈ 0,9533
        let pts = hourly(24 * 14, 0, |h| (if h < 72 { 950 } else { 1000 }, 4));
        let e = estimate(&pts, NOW, None, None).unwrap();
        let f = 0.95f64.powf(7.0 / 7.5);
        assert_eq!(e.high.price_cents, (1000.0 * f).floor() as i64);
        assert!(f > TREND_FLOOR && f < 1.0);
    }

    #[test]
    fn rise_is_not_credited() {
        let pts = hourly(24 * 14, 0, |h| (if h < 72 { 1500 } else { 1000 }, 4));
        let e = estimate(&pts, NOW, None, None).unwrap();
        assert_eq!(e.quick.price_cents, 1000); // Q25 da janela de 7 d: 1000 (96 h) contra 1500 (72 h)
        assert!(!e.evidence.contains("queda"));
        // sem histórico dos dias 4–14 (só 3 dias de dados): fator 1
        let e = estimate(&hourly(72, 0, |_| (600, 4)), NOW, None, None).unwrap();
        assert!(!e.evidence.contains("queda"));
    }

    #[test]
    fn lowest_other_ask_caps_quick_and_mid() {
        let e = estimate(&flat(1000, 5), NOW, Some(900), None).unwrap();
        assert_eq!(e.quick.price_cents, 899);
        assert_eq!((e.mid.price_cents, e.high.price_cents), (899, 1000)); // o alto segue otimista
        assert!(e.evidence.contains("preço médio limitado"));
        assert!(e.evidence.contains("limitada pelo menor anúncio"));
        // anúncio acima do Q50: não mexe
        let e = estimate(&flat(1000, 5), NOW, Some(1200), None).unwrap();
        assert_eq!((e.quick.price_cents, e.mid.price_cents), (1000, 1000));
        // anúncio de 1 centavo: ver tiny_competitor_ask_gives_no_quick_price
    }

    #[test]
    fn eta_counts_only_prices_that_paid() {
        // metade das compras a 500, metade a 1000, 10 vendas/dia
        let pts: Vec<PricePoint> = (0..70)
            .map(|i| PricePoint {
                ts: NOW - 3600 * (i + 1),
                median_cents: if i % 2 == 0 { 500 } else { 1000 },
                purchases: 1,
            })
            .collect();
        let e = estimate(&pts, NOW, None, None).unwrap();
        assert_eq!(e.high.price_cents, 1000);
        // 70 compras / 7 d = 10/dia; fração ≥ 1000 = 0,5 → 24 / 5 h
        assert!((e.high.eta_hours.unwrap() - 4.8).abs() < 1e-9);
        assert!((e.quick.eta_hours.unwrap() - 2.4).abs() < 1e-9);
    }

    #[test]
    fn rare_n_zero_two_three() {
        let base = estimate(&flat(1000, 5), NOW, None, None).unwrap();
        // n = 0 e n = 2: preço da base, evidência avisa
        for (m, txt) in [
            (&[][..], "prêmio sem prova (0 vendas)"),
            (&[1.8, 2.0][..], "prêmio sem prova (2 vendas)"),
        ] {
            let e = estimate(&flat(1000, 5), NOW, None, Some(rare(m))).unwrap();
            assert_eq!((e.quick, e.mid, e.high), (base.quick, base.mid, base.high));
            assert!(e.evidence.contains(txt), "{}", e.evidence);
        }
        assert!(
            estimate(&flat(1000, 5), NOW, None, Some(rare(&[2.0])))
                .unwrap()
                .evidence
                .contains("(1 venda)")
        );
        // n = 3: mid e alto multiplicam pelos quantis (Q50 = do meio, Q75 = maior); o rápido fica sem prêmio
        let e = estimate(&flat(1000, 5), NOW, None, Some(rare(&[2.0, 1.4, 1.6]))).unwrap();
        assert_eq!(
            (e.quick.price_cents, e.mid.price_cents, e.high.price_cents),
            (1000, 1600, 2000)
        );
        assert_eq!(e.confidence, Confidence::Medium); // n < 8 nunca é alta
        assert_eq!(e.mid.eta_hours, None);
        assert!(
            e.evidence.contains("grupo raro: 3 vendas, 1,4x a 2,0x"),
            "{}",
            e.evidence
        );
        // prêmio abaixo de 1× derruba o rápido também
        let e = estimate(&flat(1000, 5), NOW, None, Some(rare(&[0.8, 1.4, 1.6]))).unwrap();
        assert_eq!(e.quick.price_cents, 800);
        // múltiplos inválidos não contam
        let e = estimate(
            &flat(1000, 5),
            NOW,
            None,
            Some(rare(&[2.0, f64::NAN, -1.0, 0.0])),
        )
        .unwrap();
        assert!(e.evidence.contains("prêmio sem prova (1 venda)"));
    }

    #[test]
    fn absurd_prices_are_rejected_not_overflowed() {
        // múltiplo 1e308 é descartado como dado ruim: vira "prêmio sem prova", preço da base
        let e = estimate(
            &flat(1000, 5),
            NOW,
            None,
            Some(rare(&[1e308, 1e308, 1e308])),
        )
        .unwrap();
        assert_eq!(e.mid.price_cents, 1000);
        assert!(
            e.evidence.contains("prêmio sem prova (0 vendas)"),
            "{}",
            e.evidence
        );
        // múltiplo plausível mas base gigante: produto acima do teto → erro, nunca i64::MAX
        let huge = estimate(
            &flat(MAX_PRICE_CENTS, 5),
            NOW,
            None,
            Some(rare(&[2.0, 2.0, 2.0])),
        );
        assert_eq!(huge, Err(NoEstimate::Implausible));
        let huge = estimate(&flat(i64::MAX, 5), NOW, None, None);
        assert_eq!(huge, Err(NoEstimate::Implausible));
        // 100× é o limite; 101× é descartado
        let e = estimate(
            &flat(1000, 5),
            NOW,
            None,
            Some(rare(&[100.0, 100.0, 100.0])),
        )
        .unwrap();
        assert_eq!(e.mid.price_cents, 100_000);
        let e = estimate(&flat(1000, 5), NOW, None, Some(rare(&[101.0; 3]))).unwrap();
        assert_eq!(e.mid.price_cents, 1000);
        // a avaliação aguenta valores extremos sem estourar
        let ev = evaluate(
            i64::MIN,
            &est(MAX_PRICE_CENTS, MAX_PRICE_CENTS, MAX_PRICE_CENTS),
            0,
        );
        assert!(!ev.safe && !ev.listable && !ev.notify);
        let ev = evaluate(
            i64::MAX,
            &est(MAX_PRICE_CENTS, MAX_PRICE_CENTS, MAX_PRICE_CENTS),
            0,
        );
        assert!(!ev.safe && !ev.listable);
    }

    #[test]
    fn tiny_competitor_ask_gives_no_quick_price() {
        for ask in [-5, 0, 1, 3] {
            // cap = ask − 1 < 3 centavos: nenhum preço rápido bate o concorrente e ainda rende algo
            let r = estimate(&flat(1000, 5), NOW, Some(ask), None);
            assert_eq!(r, Err(NoEstimate::NoQuickPrice), "ask {ask}");
        }
        // ask 4: cap 3, quick 3 → líquido 1 centavo (> 0)
        let e = estimate(&flat(1000, 5), NOW, Some(4), None).unwrap();
        assert_eq!(e.quick.price_cents, 3);
        assert!(seller_gets(e.quick.price_cents) > 0);
        // mediana baixíssima também respeita o piso de 3 centavos
        let e = estimate(&flat(1, 5), NOW, None, None).unwrap();
        assert_eq!(e.quick.price_cents, MIN_PRICE_CENTS);
        assert!(
            NoEstimate::NoQuickPrice
                .to_string()
                .contains("barato demais")
        );
    }

    #[test]
    fn nonpositive_purchase_price_is_never_safe() {
        for p in [-1, 0, i64::MIN] {
            let ev = evaluate(p, &est(900, 1107, 1400), 0);
            assert!(!ev.safe && !ev.listable && !ev.notify, "price {p}");
            assert_eq!(ev.margin_pct, 0.0);
        }
    }

    #[test]
    fn rare_confidence_needs_eight_sales_and_base_confidence() {
        let m8 = rare(&[1.5; 8]);
        let e = estimate(&flat(1000, 16), NOW, None, Some(m8.clone())).unwrap();
        assert_eq!(e.confidence, Confidence::High);
        // base fraca puxa para baixo mesmo com n alto
        let weak = hourly(24 * 7, 0, |h| (1000, u32::from(h % 6 == 0))); // 28 compras em 7 dias
        let e = estimate(&weak, NOW, None, Some(m8.clone())).unwrap();
        assert_eq!(e.confidence, Confidence::Low);
    }

    fn est(quick: i64, mid: i64, high: i64) -> SellEstimate {
        let t = |p| EstTier {
            price_cents: p,
            eta_hours: Some(3.0),
        };
        SellEstimate {
            quick: t(quick),
            mid: t(mid),
            high: t(high),
            confidence: Confidence::High,
            probabilities: [1.0, 1.0, 1.0],
            volatility: Some(0.0),
            risk: Risk::Low,
            evidence: String::new(),
        }
    }

    #[test]
    fn daily_score_handles_missing_data_and_risk() {
        let flat_est = estimate(&flat(1000, 16), NOW, None, None).unwrap();
        let ev = evaluate(500, &flat_est, 0);
        assert_eq!(flat_est.volatility, Some(0.0));
        assert_eq!(flat_est.risk, Risk::Low);
        assert_eq!(ev.ev_profit_cents, Some(ev.sell.mid.profit_cents));
        assert!(ev.days_locked.unwrap() > 7.0);
        assert!(ev.profit_per_day_cents.unwrap() > 0);
        let mut different = est(600, 900, 1200);
        different.probabilities = [1.0, 0.5, 0.0];
        let weighted = evaluate(500, &different, 0);
        let expected = (weighted.sell.quick.profit_cents as f64 * 2.0
            + weighted.sell.mid.profit_cents as f64)
            / 3.0;
        assert_eq!(weighted.ev_profit_cents, Some(expected.round() as i64));
        different.volatility = Some(1.0);
        assert_eq!(
            evaluate(500, &different, 0).ev_profit_cents,
            Some(expected.round() as i64 - 500)
        );
        different.probabilities = [0.0; 3];
        assert_eq!(evaluate(500, &different, 0).ev_profit_cents, None);
        // dias sem vizinho: sem retornos diários, sem volatilidade
        let sparse: Vec<_> = flat(1000, 16)
            .into_iter()
            .filter(|p| [1, 3, 5].contains(&((NOW - p.ts) / DAY)) && (NOW - p.ts) % DAY == 0)
            .collect();
        assert_eq!(sparse.len(), 3);
        let sparse = estimate(&sparse, NOW, None, None).unwrap();
        assert_eq!(sparse.volatility, None);
        assert_eq!(evaluate(500, &sparse, 0).ev_profit_cents, None);
        let rare = estimate(&flat(1000, 16), NOW, None, Some(rare(&[2.0; 3]))).unwrap();
        assert_eq!(rare.risk, Risk::Medium);
        assert_eq!(evaluate(500, &rare, 0).profit_per_day_cents, None);
        let mut volatile = flat(1000, 16);
        for p in &mut volatile {
            if p.ts.div_euclid(DAY) % 2 == 0 {
                p.median_cents = 2000;
            }
        }
        assert_eq!(
            estimate(&volatile, NOW, None, None).unwrap().risk,
            Risk::High
        );
    }

    #[test]
    fn risk_adjusted_negative_profit_does_not_notify() {
        let quick = crate::pricing::fees::buyer_pays(10_100);
        let mid = crate::pricing::fees::buyer_pays(10_500);
        let mut estimate = est(quick, mid, mid);
        estimate.probabilities = [0.5, 0.5, 0.0];
        estimate.volatility = Some(0.05);
        let ev = evaluate(10_000, &estimate, 100);
        assert_eq!(ev.sell.mid.profit_cents, 500);
        assert_eq!(ev.ev_profit_cents, Some(-1023));
        assert!(ev.listable && ev.safe && !ev.notify);
        estimate.volatility = None;
        assert!(!evaluate(10_000, &estimate, 100).notify);
    }

    #[test]
    fn evaluate_flags() {
        // compra 560; mid 1107 → líquido 963 → lucro 403; quick 900 → líquido 783 → lucro 223
        let ev = evaluate(560, &est(900, 1107, 1400), 100);
        assert_eq!(ev.sell.mid.net_cents, 963); // 963+48+96 = 1107
        assert_eq!(ev.sell.mid.profit_cents, 403);
        assert_eq!(ev.sell.quick.profit_cents, 223);
        assert!((ev.margin_pct - 71.96).abs() < 0.01);
        assert!(ev.safe && ev.listable && ev.notify);
        assert_eq!(ev.sell.high.eta_hours, Some(3.0));
    }

    #[test]
    fn evaluate_borders() {
        // lucro no mid exatamente 20 e margem ≥ 5 %: lista
        let net = seller_gets(480); // 419; margem de 20 centavos sobre 399 = 5,01 %
        let ev = evaluate(net - 20, &est(480, 480, 480), 20);
        assert_eq!(ev.sell.mid.profit_cents, 20);
        assert!(ev.listable && ev.safe && ev.notify);
        // lucro 19: não lista
        let ev = evaluate(net - 19, &est(480, 480, 480), 0);
        assert!(!ev.listable && !ev.notify);
        // lucro ≥ 20 mas margem < 5 %: preço alto
        let big = 100_000;
        let ev = evaluate(big, &est(big, 120_000, 120_000), 0);
        assert!(ev.sell.mid.profit_cents >= 20 && ev.margin_pct < 5.0 && !ev.listable);
        // lucra no mid, perde no quick: não é safe, mas avisa porque o lucro esperado é positivo
        let ev = evaluate(560, &est(500, 1107, 1400), 100);
        assert!(ev.listable && !ev.safe && ev.notify);
        // lucro abaixo do mínimo do usuário: lista, não avisa
        let ev = evaluate(560, &est(900, 1107, 1400), 500);
        assert!(ev.listable && ev.safe && !ev.notify);
        // preço de compra zero ou negativo: nunca lista
        assert!(!evaluate(0, &est(900, 1107, 1400), 0).listable);
        assert!(!evaluate(-1, &est(900, 1107, 1400), 0).listable);
        // tudo abaixo do custo
        let ev = evaluate(2000, &est(500, 900, 1000), 0);
        assert!(ev.sell.high.profit_cents < 0 && !ev.listable && !ev.safe);
    }

    /// Caso real: Glock-18 Moonrise FT, T1 Center Star (Erick comprou a R$ 5,60 e vendeu a R$ 12,52 em < 2 h).
    ///
    /// Entradas: mediana comum R$ 6,63 e 392 vendas/dia (conhecimento 5.5); múltiplos T1 do CSFloat de
    /// pesquisa-web 5 (224 → 1,05/0,73, 601 → 1,49/0,73, 58 → 1,50/0,73, 864 → 1,22/0,73).
    /// O histórico por hora é SINTÉTICO (não há fixture real aqui): oscila de R$ 6,40 a R$ 6,90 em torno
    /// da mediana medida. O que vale no teste é a aritmética do modelo, não a série.
    #[test]
    fn moonrise_t1_flip_of_erick() {
        let pts = hourly(24 * 14, 0, |h| (640 + (h * 7 % 6) * 10, 16)); // 640..690, mediana ≈ 665
        let multiples = [1.05 / 0.73, 1.49 / 0.73, 1.50 / 0.73, 1.22 / 0.73]; // 1,44 / 1,67 / 2,04 / 2,05
        let e = estimate(&pts, NOW, Some(1500), Some(rare(&multiples))).unwrap();
        let ev = evaluate(560, &e, 100);
        // o modelo teria sugerido vender entre ~R$ 6,50 (sem prêmio) e ~R$ 14, com o miolo perto de R$ 11
        assert!(
            (640..=690).contains(&e.quick.price_cents),
            "quick {}",
            e.quick.price_cents
        );
        assert!(
            (1050..=1200).contains(&e.mid.price_cents),
            "mid {}",
            e.mid.price_cents
        );
        assert!(
            (1300..=1450).contains(&e.high.price_cents),
            "high {}",
            e.high.price_cents
        );
        // o preço real do Erick (R$ 12,52) ficou entre mid e high: o modelo não superestimou
        assert!(e.mid.price_cents < 1252 && 1252 < e.high.price_cents);
        // com custo R$ 5,60 o flip é seguro, entra na lista e avisa
        assert!(ev.safe && ev.listable && ev.notify, "{ev:?}");
        assert_eq!(e.confidence, Confidence::Medium); // n = 4 < 8
        // lucro realizado do Erick (R$ 5,29 em 12,52) cabe dentro do intervalo previsto [quick, high]
        let real = seller_gets(1252) - 560;
        assert_eq!(real, 529);
        assert!(ev.sell.quick.profit_cents < real && real < ev.sell.high.profit_cents);
        // prazo: com prêmio provado não há medida de ritmo do grupo
        assert_eq!(e.mid.eta_hours, None);
    }
}
