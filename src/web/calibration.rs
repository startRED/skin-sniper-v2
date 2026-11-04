//! Registro manual e números observados; a precificação ainda não usa estes dados.

use super::AppState;
use super::api::{ApiError, Kind, Opportunity, Purchase, PurchaseStatus};
use crate::db::now_unix;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Timelike};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoldInput {
    sold_cents: i64,
}

pub async fn sold(
    State(app): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<SoldInput>,
) -> Result<Json<Purchase>, ApiError> {
    if !(1..=crate::pricing::sell::MAX_PRICE_CENTS).contains(&input.sold_cents) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "Informe um valor de venda entre 1 e 10.000.000 centavos.",
        ));
    }
    let now = now_unix();
    let result = app
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let json: Option<String> = tx
                .query_row("SELECT json FROM purchases WHERE id = ?1", [&id], |r| {
                    r.get(0)
                })
                .optional()?;
            let Some(json) = json else { return Ok(None) };
            let mut p: Purchase = serde_json::from_str(&json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
            })?;
            if p.status != PurchaseStatus::Bought || p.tradable_at.is_none_or(|t| t > now) {
                return Ok(Some(Err(())));
            }
            tx.execute("DELETE FROM outcomes WHERE purchase_id = ?1", [&id])?;
            tx.execute(
                "INSERT INTO outcomes(purchase_id, sold_cents, sold_at) VALUES (?1, ?2, ?3)",
                params![id, input.sold_cents, now],
            )?;
            tx.commit()?;
            p.sold_cents = Some(input.sold_cents);
            Ok(Some(Ok(p)))
        })
        .await?;
    match result {
        None => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "Compra não encontrada.",
        )),
        Some(Err(())) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "A skin ainda não está liberada para venda.",
        )),
        Some(Ok(p)) => {
            let _ = app.events.send(super::api::Event::Purchase(p.clone()));
            Ok(Json(p))
        }
    }
}

#[derive(Serialize)]
pub struct Calibration {
    families: Vec<Family>,
    hours: Vec<Hour>,
}
#[derive(Serialize)]
struct Family {
    family: String,
    kind: &'static str,
    n: u64,
    mean_error_pct: f64,
    bias_pct: f64,
}
#[derive(Default, Serialize)]
struct Hour {
    hour: u32,
    listings: i64,
    opportunities: i64,
}

pub async fn get(State(app): State<AppState>) -> Result<Json<Calibration>, ApiError> {
    let since = (now_unix() - 30 * 86_400).div_euclid(3600);
    let (sales, activity) = app.db.call(move |c| {
        let sales = c.prepare("SELECT p.json, o.sold_cents, (SELECT json FROM opportunities WHERE id = p.id) FROM purchases p JOIN outcomes o ON o.rowid = (SELECT rowid FROM outcomes WHERE purchase_id = p.id ORDER BY rowid DESC LIMIT 1)")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<String>>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let activity = c.prepare("SELECT hour_utc, listings, opportunities FROM hourly_activity WHERE hour_utc >= ?1 AND hour_utc <= ?2")?
            .query_map(params![since, now_unix().div_euclid(3600)], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((sales, activity))
    }).await?;
    let mut groups: BTreeMap<(String, &'static str), (u64, f64, f64)> = BTreeMap::new();
    for (json, sold, opportunity) in sales {
        let Ok(p) = serde_json::from_str::<Purchase>(&json) else {
            continue;
        };
        let kind = p.kind.or_else(|| {
            opportunity
                .and_then(|j| serde_json::from_str::<Opportunity>(&j).ok())
                .map(|o| o.kind)
        });
        let Some(kind) = kind else { continue };
        let predicted = p.sell.mid.price_cents;
        if predicted <= 0 || sold <= 0 {
            continue;
        }
        let family = [
            " (Factory New)",
            " (Minimal Wear)",
            " (Field-Tested)",
            " (Well-Worn)",
            " (Battle-Scarred)",
        ]
        .iter()
        .find_map(|suffix| p.name.strip_suffix(suffix))
        .unwrap_or(&p.name)
        .to_string();
        let key = (
            family,
            match kind {
                Kind::Desconto => "desconto",
                Kind::Raro => "raro",
            },
        );
        let error = (sold as f64 - predicted as f64) / predicted as f64 * 100.0;
        let entry = groups.entry(key).or_default();
        entry.0 += 1;
        entry.1 += error.abs();
        entry.2 += error;
    }
    let families = groups
        .into_iter()
        .map(|((family, kind), (n, abs, signed))| Family {
            family,
            kind,
            n,
            mean_error_pct: abs / n as f64,
            bias_pct: signed / n as f64,
        })
        .collect();
    let mut hours: Vec<Hour> = (0..24)
        .map(|hour| Hour {
            hour,
            ..Hour::default()
        })
        .collect();
    for (hour, listings, opportunities) in activity {
        if let Some(h) = sao_paulo_hour(hour) {
            hours[h].listings += listings;
            hours[h].opportunities += opportunities;
        }
    }
    Ok(Json(Calibration { families, hours }))
}

fn sao_paulo_hour(hour: i64) -> Option<usize> {
    let dt = hour
        .checked_mul(3600)
        .and_then(|ts| DateTime::from_timestamp(ts, 0))?;
    Some(dt.with_timezone(&chrono_tz::America::Sao_Paulo).hour() as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sao_paulo_respects_historical_summer_time() {
        let utc = |s| {
            DateTime::parse_from_rfc3339(s)
                .unwrap()
                .timestamp()
                .div_euclid(3600)
        };
        assert_eq!(sao_paulo_hour(utc("2018-11-04T03:00:00Z")), Some(1));
        assert_eq!(sao_paulo_hour(utc("2026-11-04T12:00:00Z")), Some(9));
    }
}
