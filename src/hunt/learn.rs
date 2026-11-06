//! Aprendizado da agenda: quanto cada anúncio novo ensina (crédito por proximidade de dar lucro).
//! Docs/projeto.md, "Caça", "Agenda que aprende".

use super::Hunt;
use super::catalog::min_profit;

/// Escala τ da proximidade, em fração do preço, enquanto não há avaliações para medir (hipótese: 3 pontos,
/// medidos em 4 850 anúncios de 04–06/11).
pub(crate) const DEFAULT_SCALE: f64 = 0.03;

/// O que um anúncio novo que não passou rende à agenda: lucro mínimo × e^(g/τ), com g = (lucro no médio −
/// lucro mínimo) ÷ preço limitado a 0. Contínuo em g = 0, onde vale o que o achado renderia.
pub(crate) fn closeness_credit(price_cents: i64, mid_profit_cents: i64, scale: f64) -> f64 {
    let need = min_profit(price_cents);
    let gap = (mid_profit_cents - need) as f64 / price_cents.max(1) as f64;
    need as f64 * (gap.min(0.0) / scale).exp()
}

/// Avaliações lidas para medir a cauda e o mínimo para confiar nela (hipóteses de calibração).
const SCALE_SAMPLE: i64 = 5_000;
const SCALE_MIN_ROWS: usize = 200;
/// Limites de τ: sem espalhamento na cauda o crédito viraria um degrau; largo demais, tudo renderia igual.
const SCALE_RANGE: (f64, f64) = (0.01, 0.10);

/// τ = excesso médio de g acima do percentil 90 (picos acima de um limiar); `None` com poucas avaliações.
pub(crate) fn tail_scale(gaps: &mut [f64]) -> Option<f64> {
    if gaps.len() < SCALE_MIN_ROWS {
        return None;
    }
    gaps.sort_by(f64::total_cmp);
    let x0 = gaps[(0.9 * (gaps.len() - 1) as f64).floor() as usize];
    let tail = &gaps[gaps.partition_point(|&g| g < x0)..];
    let mean = tail.iter().map(|g| g - x0).sum::<f64>() / tail.len() as f64;
    Some(mean.clamp(SCALE_RANGE.0, SCALE_RANGE.1))
}

/// Mede τ nas últimas avaliações com estimativa; sem dados suficientes, volta ao padrão.
pub(crate) async fn refresh_scale(h: &Hunt) -> anyhow::Result<()> {
    let rows: Vec<(i64, i64)> =
        h.db.call(|c| {
            c.prepare(
                "SELECT price_cents, mid_profit_cents FROM evaluations
                 WHERE mid_profit_cents IS NOT NULL ORDER BY id DESC LIMIT ?1",
            )?
            .query_map([SCALE_SAMPLE], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
        })
        .await?;
    let mut gaps: Vec<f64> = rows
        .iter()
        .map(|&(price, mid)| (mid - min_profit(price)) as f64 / price.max(1) as f64)
        .collect();
    let scale = tail_scale(&mut gaps).unwrap_or(DEFAULT_SCALE);
    let mut s = h.lock();
    if (scale - s.closeness_scale).abs() > 0.01 * s.closeness_scale {
        s.closeness_scale = scale;
        drop(s);
        tracing::info!(
            escala = scale,
            avaliacoes = rows.len(),
            "escala de proximidade da agenda"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closeness_is_continuous_and_falls_by_e_per_scale() {
        // R$ 20,00: lucro mínimo = R$ 2,00 (10 %); faltou 0 → 200; faltou 3 pontos (60 c) → 200/e
        assert_eq!(closeness_credit(2000, 200, 0.03), 200.0);
        assert_eq!(closeness_credit(2000, 900, 0.03), 200.0); // passou da régua: teto no lucro mínimo
        let one_scale = closeness_credit(2000, 140, 0.03);
        assert!(
            (one_scale - 200.0 / std::f64::consts::E).abs() < 1e-9,
            "{one_scale}"
        );
        assert!(closeness_credit(2000, -500, 0.03) < 0.01);
        let tiny = closeness_credit(1, 0, 0.03); // preço de 1 centavo: finito
        assert!(tiny.is_finite() && tiny >= 0.0);
    }

    #[test]
    fn tail_scale_is_the_mean_excess_over_the_90th_percentile() {
        assert_eq!(tail_scale(&mut vec![-0.2; 199]), None); // < 200 avaliações
        // 0,9 × 999 = 899,1 → p90 = gaps[899] = −0,101; excesso dos 101 do topo: média de 0..=100 milésimos = 0,05
        let mut gaps: Vec<f64> = (0..1000).map(|i| -1.0 + f64::from(i) / 1000.0).collect();
        let tau = tail_scale(&mut gaps).unwrap();
        assert!((tau - 0.05).abs() < 1e-9, "{tau}");
        assert_eq!(tail_scale(&mut vec![-0.3; 500]), Some(0.01)); // cauda sem espalhamento: piso
        let mut wide: Vec<f64> = (0..1000).map(|i| -10.0 + f64::from(i) / 50.0).collect();
        assert_eq!(tail_scale(&mut wide), Some(0.10)); // teto
    }
}
