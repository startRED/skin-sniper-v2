//! `GET /api/learning`: o placar diário do aprendizado (mina de ouro). Lê o registro de observações, o placar
//! por hora e o resto do que o professor guarda; nada aqui fala com a Steam. Dia = dia de Brasília (UTC−3).

use super::AppState;
use super::api::ApiError;
use crate::db::now_unix;
use axum::{Json, extract::State};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const DAY: i64 = 86_400;
/// Brasília não tem horário de verão desde 2019.
const BRT: i64 = -3 * 3600;
/// Dias no placar.
pub const DAYS: i64 = 14;

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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Learning {
    /// Do mais recente ao mais antigo.
    pub days: Vec<Day>,
    /// Primeiro anúncio do registro (`None` = registro vazio).
    pub since: Option<i64>,
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
            Ok(Learning {
                days: scorecard(now, &hours, &new, &gone),
                since,
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
            ((now - 30 * DAY) / 3600, 99, 99, 99, 99), // fora dos 14 dias
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
}
