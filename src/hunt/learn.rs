//! Aprendizado da agenda: quanto cada anúncio novo ensina (crédito por proximidade de dar lucro).
//! Docs/projeto.md, "Caça", "Agenda que aprende".

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
}
