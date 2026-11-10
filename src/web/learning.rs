//! `GET /api/learning`: o placar diário do aprendizado (mina de ouro). Lê o registro de observações, o placar
//! por hora e o resto do que o professor guarda; nada aqui fala com a Steam. Dia = dia de Brasília (UTC−3).

use super::AppState;
use super::api::ApiError;
use crate::db::now_unix;
use axum::{Json, extract::State};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const DAY: i64 = 86_400;
/// Brasília não tem horário de verão desde 2019.
const BRT: i64 = -3 * 3600;
/// Dias no placar.
pub const DAYS: i64 = 60;

/// Dia de Brasília de um instante (número de dias desde 1970).
pub fn brt_day(ts: i64) -> i64 {
    (ts + BRT).div_euclid(DAY)
}

fn day_label(d: i64) -> String {
    chrono::DateTime::from_timestamp(d * DAY, 0)
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Day {
    /// `AAAA-MM-DD` em Brasília.
    pub day: String,
    /// Leituras de anúncios e do livro de ofertas.
    pub queries: i64,
    pub books: i64,
    /// Anúncios vistos pela primeira vez; os de idade conhecida e a idade mediana deles ao serem vistos.
    pub new_listings: i64,
    pub aged: i64,
    pub age_median_min: Option<f64>,
    /// Sumiram sem relistagem (venda, ou retirada sem voltar) e relistados pelo mesmo vendedor.
    pub sales: i64,
    pub relists: i64,
    /// Vida mediana dos que sumiram sem relistagem, do primeiro instante visto ao sumiço.
    pub life_median_min: Option<f64>,
    /// Quase-achados e achados.
    pub near: i64,
    pub finds: i64,
    /// Avisos comuns com preço observado nas 48 h, agrupados pela data da previsão.
    pub price_samples: i64,
    pub price_error_pct: Option<f64>,
    pub price_bias_pct: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Learning {
    /// Do mais recente ao mais antigo.
    pub days: Vec<Day>,
    /// Primeiro anúncio do registro (`None` = registro vazio).
    pub since: Option<i64>,
    /// Compras virtuais e o que o mercado disse delas.
    #[serde(default)]
    pub teacher: crate::hunt::teacher::Summary,
    /// Agenda: parâmetros do valor de uma leitura e medidas (`None` até o primeiro ajuste).
    #[serde(default)]
    pub value: Option<crate::hunt::value::Info>,
    /// Fator do professor no preço do comum (1 = sem desconto).
    #[serde(default = "one")]
    pub refit: f64,
    /// Boletim: últimos 7 dias e os 7 anteriores.
    #[serde(default)]
    pub weeks: [crate::hunt::teacher::Week; 2],
    #[serde(default)]
    pub research: Option<crate::research::Summary>,
    #[serde(default)]
    pub trial: Option<crate::hunt::trial::Info>,
    #[serde(default)]
    pub cohorts: Vec<crate::hunt::teacher::Cohort>,
    #[serde(default)]
    pub exploration: Option<crate::hunt::explore::Info>,
    #[serde(default)]
    pub experiment: Option<crate::hunt::trial::Report>,
    /// Alterações efetivamente aplicadas ao preço dos comuns.
    #[serde(default)]
    pub adjustments: Vec<Adjustment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Adjustment {
    pub at: i64,
    pub old: Option<f64>,
    pub new: Option<f64>,
    pub why: String,
}

fn one() -> f64 {
    1.0
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

/// Linha do placar por hora: (hora UTC, consultas, livros, quase, achados).
pub type HourRow = (i64, i64, i64, i64, i64);

/// Monta os dias a partir das linhas cruas (puro, para teste).
/// - `new`: (primeiro instante, idade máxima) dos anúncios novos;
/// - `gone`: (sumiço, primeiro instante, relistado).
pub fn scorecard(
    now: i64,
    hours: &[HourRow],
    new: &[(i64, Option<i64>)],
    gone: &[(i64, i64, bool)],
) -> Vec<Day> {
    let today = brt_day(now);
    let mut by: BTreeMap<i64, (Day, Vec<f64>, Vec<f64>)> = (today - DAYS + 1..=today)
        .map(|d| {
            (
                d,
                (
                    Day {
                        day: day_label(d),
                        ..Day::default()
                    },
                    vec![],
                    vec![],
                ),
            )
        })
        .collect();
    for &(hour, queries, books, near, finds) in hours {
        if let Some((d, ..)) = by.get_mut(&brt_day(hour * 3600)) {
            d.queries += queries;
            d.books += books;
            d.near += near;
            d.finds += finds;
        }
    }
    for &(first, age) in new {
        if let Some((d, ages, _)) = by.get_mut(&brt_day(first)) {
            d.new_listings += 1;
            if let Some(a) = age {
                d.aged += 1;
                ages.push(a as f64 / 60.0);
            }
        }
    }
    for &(at, first, relisted) in gone {
        if let Some((d, _, lives)) = by.get_mut(&brt_day(at)) {
            if relisted {
                d.relists += 1;
            } else {
                d.sales += 1;
                lives.push((at - first) as f64 / 60.0);
            }
        }
    }
    by.into_values()
        .rev()
        .map(|(mut d, mut ages, mut lives)| {
            d.age_median_min = median(&mut ages);
            d.life_median_min = median(&mut lives);
            d
        })
        .collect()
}

/// Cada erro compara o preço observado ao previsto; não representa uma venda confirmada.
fn price_scorecard(days: &mut [Day], samples: &[(i64, i64, i64)]) {
    let mut by_day: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
    for &(at, predicted, observed) in samples {
        if predicted > 0 && observed > 0 {
            by_day
                .entry(brt_day(at))
                .or_default()
                .push((observed as f64 - predicted as f64) / predicted as f64 * 100.0);
        }
    }
    for day in days {
        if let Ok(date) = chrono::NaiveDate::parse_from_str(&day.day, "%Y-%m-%d") {
            let key = date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() / DAY;
            if let Some(values) = by_day.get(&key) {
                let mut signed = values.clone();
                let mut absolute: Vec<f64> = values.iter().map(|v| v.abs()).collect();
                day.price_samples = values.len() as i64;
                day.price_bias_pct = median(&mut signed);
                day.price_error_pct = median(&mut absolute);
            }
        }
    }
}

pub async fn get(State(app): State<AppState>) -> Result<Json<Learning>, ApiError> {
    let now = now_unix();
    let from = (brt_day(now) - DAYS + 1) * DAY - BRT;
    let out = app
        .db
        .call(move |c| {
            let hours: Vec<HourRow> = c
                .prepare(
                    "SELECT hour_utc, queries, books, near, opportunities FROM hourly_activity WHERE hour_utc >= ?1",
                )?
                .query_map([from.div_euclid(3600)], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            let new: Vec<(i64, Option<i64>)> = c
                .prepare("SELECT first_seen, age_max FROM observations WHERE first_seen >= ?1")?
                .query_map([from], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let gone: Vec<(i64, i64, bool)> = c
                .prepare(
                    "SELECT gone_at, first_seen, relisted FROM observations WHERE gone_at >= ?1",
                )?
                .query_map(params![from], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let since: Option<i64> =
                c.query_row("SELECT MIN(first_seen) FROM observations", [], |r| r.get(0))?;
            let prices: Vec<(i64, i64, i64)> = c
                .prepare("SELECT bought_at, mid_price_cents, fast_median_cents FROM ghosts WHERE bought_at >= ?1 AND grp IS NULL AND approved = 1 AND fast_at IS NOT NULL AND fast_median_cents > 0 AND mid_price_cents > 0")?
                .query_map([from], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut days = scorecard(now, &hours, &new, &gone);
            price_scorecard(&mut days, &prices);
            let adjustments = c.prepare("SELECT at, old, new, why FROM refit_log WHERE key = 'comum' AND at >= ?1 ORDER BY at DESC")?
                .query_map([from], |r| Ok(Adjustment { at: r.get(0)?, old: r.get(1)?, new: r.get(2)?, why: r.get(3)? }))?
                .collect::<rusqlite::Result<_>>()?;
            let value: Option<String> = c
                .query_row(
                    "SELECT value FROM settings WHERE key = ?1",
                    [crate::hunt::value::INFO_KEY],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(Learning {
                days,
                since,
                teacher: crate::hunt::teacher::summary(c)?,
                value: value.and_then(|v| serde_json::from_str(&v).ok()),
                refit: crate::hunt::teacher::refit_factor(c)?,
                weeks: [
                    crate::hunt::teacher::week(c, now - 7 * DAY, now)?,
                    crate::hunt::teacher::week(c, now - 14 * DAY, now - 7 * DAY)?,
                ],
                research: Some(crate::research::summary(c, now)?),
                trial: Some(crate::hunt::trial::info(c, now)?),
                cohorts: crate::hunt::teacher::cohorts(c, now - 60 * DAY, now, now)?,
                exploration: Some(crate::hunt::explore::info(c)?),
                experiment: Some(crate::hunt::trial::report(c, now)?),
                adjustments,
            })
        })
        .await?;
    Ok(Json(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learning_scorecard_counts_by_brasilia_day() {
        // 2026-11-06 12:00 em Brasília = 15:00 UTC
        let now = 1_793_977_200;
        assert_eq!(day_label(brt_day(now)), "2026-11-06");
        // 01:30 UTC do dia 06 ainda é dia 05 em Brasília
        let late_5th = now - 13 * 3600 - 30 * 60;
        assert_eq!(day_label(brt_day(late_5th)), "2026-11-05");
        let hours = [
            (now / 3600, 10, 4, 2, 1),
            (late_5th / 3600, 7, 0, 1, 0),
            ((now - 70 * DAY) / 3600, 99, 99, 99, 99), // fora dos 60 dias
        ];
        let new = [
            (now, Some(120)),
            (now - 60, Some(360)),
            (now - 90, None),
            (late_5th, Some(60)),
        ];
        let gone = [
            (now, now - 600, false),
            (now, now - 1800, false),
            (now, now - 60, true),
        ];
        let days = scorecard(now, &hours, &new, &gone);
        assert_eq!(days.len(), DAYS as usize);
        let d = &days[0];
        assert_eq!(d.day, "2026-11-06");
        assert_eq!((d.queries, d.books, d.near, d.finds), (10, 4, 2, 1));
        assert_eq!(
            (d.new_listings, d.aged, d.age_median_min),
            (3, 2, Some(4.0))
        );
        assert_eq!((d.sales, d.relists, d.life_median_min), (2, 1, Some(20.0)));
        let y = &days[1];
        assert_eq!(
            (y.day.as_str(), y.queries, y.new_listings),
            ("2026-11-05", 7, 1)
        );
        assert_eq!(days[2].age_median_min, None);
        assert_eq!(days.iter().map(|d| d.queries).sum::<i64>(), 17);
    }

    #[test]
    fn price_scorecard_uses_prediction_day_and_keeps_error_separate_from_bias() {
        let now = 1_793_977_200;
        let mut days = scorecard(now, &[], &[], &[]);
        price_scorecard(
            &mut days,
            &[(now, 1000, 900), (now, 1000, 1100), (now, 0, 100)],
        );
        assert_eq!(days[0].price_samples, 2);
        assert_eq!(days[0].price_error_pct, Some(10.0));
        assert_eq!(days[0].price_bias_pct, Some(0.0));
        assert_eq!(days[1].price_error_pct, None);
    }
}
