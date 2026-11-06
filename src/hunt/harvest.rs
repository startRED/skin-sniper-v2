//! Colheita (mina de ouro, Fase 3): consultar o alvo onde mais achado se acumulou desde a última consulta.
//!
//! Achado chega a uma taxa λ por alvo e some (outro compra) com vida média `L`. O estoque que sobra `Δ` segundos
//! depois da última consulta é `λ·L·(1 − e^(−Δ/L))`. λ = recompensa ÷ exposição, as duas com o decaimento de 24 h
//! da agenda; cada consulta soma `L·(1 − e^(−Δ/L))` de exposição. `L` = vida mediana dos anúncios que eram o mais
//! barato e sumiram sem relistagem (registro), padrão 600 s.
//!
//! A política muda por dia de Brasília, contado da primeira volta deste código (`harvest_start` em `settings`):
//! 7 dias em sombra (o UCB1 escolhe, a colheita só mede), 7 dias alternando (par = UCB1, ímpar = colheita) e depois
//! fica a que rendeu mais (achados + quase-achados) por consulta nesses 7 dias. A exploração não muda.
//! Prazos e o padrão de 600 s são hipóteses de calibração. Docs/projeto.md, "Aprendizado".

use super::Hunt;
use super::catalog::{get_setting, set_setting};
use crate::web::learning::brt_day;
use serde::{Deserialize, Serialize};

const DAY: i64 = 86_400;
/// Vida padrão do achado sem medida suficiente.
pub const DEFAULT_LIFE: f64 = 600.0;
/// Sumiços medidos para trocar o padrão pela mediana.
const MIN_LIVES: usize = 20;
const SHADOW_DAYS: i64 = 7;
const TRIAL_DAYS: i64 = 7;
const START_KEY: &str = "harvest_start";
/// Resumo da colheita para o placar (JSON em `settings`).
pub const INFO_KEY: &str = "harvest_info";
const GAP: i64 = 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    /// O UCB1 escolhe; a colheita só mede.
    #[default]
    Shadow,
    Ucb,
    Harvest,
}

/// Exposição de uma consulta feita `gap` segundos depois da anterior do mesmo alvo (`None` = primeira).
pub fn exposure(gap: Option<i64>, life: f64) -> f64 {
    match gap {
        Some(d) => life * (1.0 - (-(d.max(0) as f64) / life).exp()),
        None => life,
    }
}

/// Recompensa esperada de consultar agora; sem medida, infinito (alvo novo, como no UCB1).
pub fn value(reward: f64, exposed: f64, since_last: i64, life: f64) -> f64 {
    if exposed <= 0.0 {
        return f64::INFINITY;
    }
    reward / exposed * exposure(Some(since_last), life)
}

/// Política do dia de `now`.
pub fn policy(start: i64, now: i64, winner: Option<Policy>) -> Policy {
    let day = brt_day(now);
    match day - brt_day(start) {
        d if d < SHADOW_DAYS => Policy::Shadow,
        d if d < SHADOW_DAYS + TRIAL_DAYS => {
            if day.rem_euclid(2) == 0 {
                Policy::Ucb
            } else {
                Policy::Harvest
            }
        }
        _ => winner.unwrap_or(Policy::Ucb),
    }
}

/// Rendimento por consulta de cada política nos dias alternados: `days` = (dia, consultas, quase, achados).
pub fn rates(start: i64, days: &[(i64, i64, i64, i64)]) -> (Option<f64>, Option<f64>) {
    let first = brt_day(start) + SHADOW_DAYS;
    let mut acc = [(0, 0); 2];
    for &(day, queries, near, finds) in days
        .iter()
        .filter(|d| d.0 >= first && d.0 < first + TRIAL_DAYS)
    {
        let a = &mut acc[usize::from(day.rem_euclid(2) == 1)];
        a.0 += near + finds;
        a.1 += queries;
    }
    let rate = |(y, q): (i64, i64)| (q > 0).then(|| y as f64 / q as f64);
    (rate(acc[0]), rate(acc[1]))
}

/// A colheita só vence medindo mais por consulta; empate ou falta de dado fica com o UCB1.
pub fn winner(rates: (Option<f64>, Option<f64>)) -> Option<Policy> {
    match rates {
        (Some(u), Some(h)) => Some(if h > u { Policy::Harvest } else { Policy::Ucb }),
        _ => None,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Info {
    pub policy: Policy,
    pub start: i64,
    /// Vida do achado em uso e quantos sumiços a mediram (0 = padrão).
    pub life_s: f64,
    pub lives: u32,
    /// Escolhas fora da exploração desde que o app subiu e quantas a colheita faria igual ao UCB1.
    pub picks: u64,
    pub agree: u64,
    /// (achados + quase) por consulta nos dias alternados.
    pub ucb_rate: Option<f64>,
    pub harvest_rate: Option<f64>,
}

fn median(v: &mut [i64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2] as f64
    } else {
        (v[n / 2 - 1] + v[n / 2]) as f64 / 2.0
    })
}

/// De hora em hora: mede `L`, decide a política do dia e grava o resumo. Só o banco, nunca a Steam.
pub async fn maybe_step(h: &Hunt) {
    let now = h.now();
    {
        let mut s = h.lock();
        if s.harvest_at != 0 && now - s.harvest_at < GAP {
            return;
        }
        s.harvest_at = now;
    }
    if let Err(e) = step(h, now).await {
        tracing::warn!("colheita: resumo falhou: {e:#}");
    }
}

async fn step(h: &Hunt, now: i64) -> anyhow::Result<()> {
    let start = match get_setting(&h.db, START_KEY).await? {
        Some(v) => v.parse().unwrap_or(now),
        None => {
            set_setting(&h.db, START_KEY, now.to_string()).await?;
            now
        }
    };
    let since = now - 7 * DAY;
    let from_hour = (brt_day(start) * DAY).div_euclid(3600);
    let (mut lives, hours) = h
        .db
        .call(move |c| {
            let lives: Vec<i64> = c
                .prepare(
                    "SELECT gone_at - first_seen FROM observations WHERE cheapest = 1 AND relisted = 0
                     AND gone_at >= ?1",
                )?
                .query_map([since], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let hours: Vec<(i64, i64, i64, i64)> = c
                .prepare(
                    "SELECT hour_utc, queries, near, opportunities FROM hourly_activity WHERE hour_utc >= ?1",
                )?
                .query_map([from_hour], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok((lives, hours))
        })
        .await?;
    let measured = lives.len() >= MIN_LIVES;
    let life = if measured {
        median(&mut lives).unwrap_or(DEFAULT_LIFE).max(60.0)
    } else {
        DEFAULT_LIFE
    };
    let days: Vec<_> = hours
        .iter()
        .map(|&(hour, q, near, finds)| (brt_day(hour * 3600), q, near, finds))
        .collect();
    let r = rates(start, &days);
    let saved = h.lock().trial.as_ref().map(|t| t.base_policy.clone());
    let p = saved
        .and_then(|p| serde_json::from_value(serde_json::Value::String(p)).ok())
        .unwrap_or_else(|| policy(start, now, winner(r)));
    let (old, picks, agree) = {
        let mut s = h.lock();
        let old = s.queue.policy;
        s.queue.life = life;
        s.queue.policy = p;
        (old, s.queue.shadow.0, s.queue.shadow.1)
    };
    if old != p {
        tracing::info!(politica = ?p, vida_s = life, "colheita: política do dia");
    }
    let info = Info {
        policy: p,
        start,
        life_s: life,
        lives: if measured { lives.len() as u32 } else { 0 },
        picks,
        agree,
        ucb_rate: r.0,
        harvest_rate: r.1,
    };
    set_setting(&h.db, INFO_KEY, serde_json::to_string(&info)?).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harvest_value_grows_then_saturates() {
        let l = DEFAULT_LIFE;
        // 100 centavos em 600 s de exposição = 1/6 por segundo
        assert_eq!(value(100.0, 600.0, 0, l), 0.0);
        let one_life = value(100.0, 600.0, 600, l);
        assert!((one_life - 100.0 * (1.0 - (-1f64).exp())).abs() < 1e-9);
        let late = value(100.0, 600.0, 6000, l);
        assert!(late > one_life && late < 100.0 && 100.0 - late < 0.01);
        assert!(value(100.0, 600.0, 60, l) < value(100.0, 600.0, 120, l));
        assert_eq!(value(0.0, 0.0, 10, l), f64::INFINITY);
        // primeira consulta conta uma vida inteira de exposição
        assert_eq!(exposure(None, l), l);
        assert!((exposure(Some(600), l) - l * (1.0 - (-1f64).exp())).abs() < 1e-9);
    }

    #[test]
    fn policy_by_day_after_shadow() {
        // 2026-11-06 15:00 UTC = meio-dia em Brasília
        let start = 1_793_977_200;
        let at = |d: i64| start + d * DAY;
        assert_eq!(policy(start, at(0), None), Policy::Shadow);
        assert_eq!(policy(start, at(6), None), Policy::Shadow);
        let d7 = brt_day(at(7));
        let (even, odd) = if d7 % 2 == 0 { (7, 8) } else { (8, 7) };
        assert_eq!(policy(start, at(even), None), Policy::Ucb);
        assert_eq!(policy(start, at(odd), None), Policy::Harvest);
        assert_eq!(policy(start, at(14), None), Policy::Ucb);
        assert_eq!(
            policy(start, at(20), Some(Policy::Harvest)),
            Policy::Harvest
        );
        // colheita rende 30 / 1000 contra 20 / 1000 do UCB1; dia da sombra e depois do teste não contam
        let s = brt_day(start);
        let days = [
            (s, 1000, 900, 0),
            (brt_day(at(even)), 1000, 18, 2),
            (brt_day(at(odd)), 600, 15, 3),
            (brt_day(at(odd + 2)), 400, 12, 0),
            (s + 14, 1000, 900, 0),
        ];
        let r = rates(start, &days);
        assert_eq!(r, (Some(0.02), Some(0.03)));
        assert_eq!(winner(r), Some(Policy::Harvest));
        assert_eq!(winner((Some(0.02), Some(0.02))), Some(Policy::Ucb));
        assert_eq!(winner((None, Some(0.5))), None);
    }
}
