//! Prêmio esperado de um seed, com limite inferior, no lugar do "provado ou não". Puro: as razões chegam de fora.
//!
//! Cada venda vira uma razão `preço ÷ preço comum do nome` (`hunt::patterns::comps`); aqui valem o log delas.
//! O prêmio é encolhido em cadeia, do geral para o específico: neutro (1×) → resto da skin → tabela externa (se
//! houver) → seeds irmãos (mesmo grupo/tier, ou nota vizinha) → vendas do próprio seed. Cada nível é um
//! estimador normal-normal com `k` vendas fictícias do nível de cima, então a média e a variância saem em conta
//! fechada: com pouca prova a média fica perto do pai e a incerteza grande, e o limite inferior (média − `Z` ×
//! desvio) cai. Quem compra olha o limite inferior; a média só serve para dizer se vale caçar o seed.
//!
//! A saída (`realistic`) corta o prêmio previsto em 15 %: a revenda mediana fica 8,3 % abaixo da venda anterior
//! do mesmo seed (CSFloat, 13/11/2026), e cai mais com o grupo em queda nos últimos 30 dias.
//!
//! `SIGMA`, `Z`, `K`, o corte e os 3 anúncios da tabela externa são hipóteses de calibração: a medida fora da
//! amostra sai do teste `catalog_on_a_real_db`.

use super::fees::seller_gets;
use super::sell::MIN_LIST_MARGIN_PCT;
use std::collections::BTreeMap;

/// Desvio do log da razão de uma venda em torno da média do seed. Dentro do mesmo seed, na mesma amostra, a
/// mediana entre 96 skins é 0,18 (13/11/2026, `catalog_on_a_real_db`), mas fora da amostra (outro desgaste, outra
/// semana) o dobro cobre melhor: com 0,35 o real ficou abaixo do limite inferior em 6,5 % das 325 vendas
/// previstas, para 5 % nominais.
pub const SIGMA: f64 = 0.35;
/// Limite inferior unilateral de 95 %.
const Z: f64 = 1.64;
/// Limite de cima do intervalo (só mostra o preço alto): 1 desvio.
const Z_UP: f64 = 1.0;
/// Vendas fictícias do nível de cima em cada nível; é o encolhimento de `premium::K`.
pub const K: f64 = 5.0;
/// Com efeito de seed significativo na skin, o encolhimento pode cair até aqui.
const K_MIN: f64 = 1.0;
/// Uma linha da tabela externa vale tantas vendas.
const EXTERNAL_SALES: f64 = 3.0;
/// Corte do prêmio previsto na saída.
pub const EXIT_CUT: f64 = 0.15;
/// Grupo em queda: o prêmio previsto não passa a menos que isto.
const TREND_FLOOR: f64 = 0.7;
/// Seeds com 2+ vendas para testar o efeito do seed; embaralhamentos do teste.
const MIN_REPEATED_SEEDS: usize = 5;
const PERMUTATIONS: usize = 200;
const SIGNIFICANT: f64 = 0.05;

/// Razões (preço ÷ comum) de cada nível, sem repetir venda entre níveis.
#[derive(Debug, Default, Clone, Copy)]
pub struct Evidence<'a> {
    pub own: &'a [f64],
    pub group: &'a [f64],
    pub skin: &'a [f64],
    /// Prêmio da tabela externa para o seed, se houver.
    pub external: Option<f64>,
}

/// Razões (preço ÷ comum): esperado, limite inferior e limite de cima.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Posterior {
    pub mean: f64,
    pub lower: f64,
    pub upper: f64,
}

#[derive(Clone, Copy)]
struct Gauss {
    mu: f64,
    var: f64,
}

fn update(parent: Gauss, ratios: &[f64], k: f64) -> Gauss {
    let n = ratios.len() as f64;
    let sum: f64 = ratios.iter().map(|r| r.ln()).sum();
    let w = k / (n + k);
    Gauss {
        mu: (sum + k * parent.mu) / (n + k),
        var: SIGMA * SIGMA / (n + k) + w * w * parent.var,
    }
}

/// Prêmio encolhido do seed. `k` = vendas fictícias por nível (`K`, ou `seed_k` da skin). Razão ≤ 0 ou não
/// finita é dado ruim e não entra.
pub fn posterior(e: &Evidence, k: f64) -> Posterior {
    let clean = |v: &[f64]| -> Vec<f64> {
        v.iter()
            .copied()
            .filter(|r| r.is_finite() && *r > 0.0)
            .collect()
    };
    let mut g = Gauss {
        mu: 0.0,
        var: SIGMA * SIGMA / k,
    };
    g = update(g, &clean(e.skin), k);
    if let Some(x) = e.external.filter(|x| x.is_finite() && *x > 0.0) {
        let (a, b) = (1.0 / g.var, EXTERNAL_SALES / (SIGMA * SIGMA));
        g = Gauss {
            mu: (a * g.mu + b * x.ln()) / (a + b),
            var: 1.0 / (a + b),
        };
    }
    g = update(g, &clean(e.group), k);
    g = update(g, &clean(e.own), k);
    let sd = g.var.sqrt();
    Posterior {
        mean: g.mu.exp(),
        lower: (g.mu - Z * sd).exp(),
        upper: (g.mu + Z_UP * sd).exp(),
    }
}

/// Razão que se espera realizar na revenda: o prêmio (o que passa de 1×) sai cortado em `EXIT_CUT` e pela
/// tendência do grupo (`trend` em (0, 1]); abaixo de 1× fica como está.
pub fn realistic(ratio: f64, trend: f64) -> f64 {
    if ratio > 1.0 {
        1.0 + (ratio - 1.0) * (1.0 - EXIT_CUT) * trend.clamp(TREND_FLOOR, 1.0)
    } else {
        ratio
    }
}

/// `[limite inferior, esperado, limite de cima]` já realistas: o que `RareEvidence` carrega.
pub fn triple(p: &Posterior, trend: f64) -> [f64; 3] {
    [p.lower, p.mean, p.upper].map(|r| realistic(r, trend))
}

/// Comprar no preço comum e vender a `ratio`× rende a margem mínima de lista depois da taxa da Steam.
pub fn clears_margin(ratio: f64) -> bool {
    const BASE: f64 = 10_000.0;
    let sale = (BASE * ratio).floor();
    sale.is_finite()
        && sale < i64::MAX as f64
        && seller_gets(sale as i64) as f64 >= BASE * (1.0 + MIN_LIST_MARGIN_PCT / 100.0)
}

/// A tabela externa mostra menos da metade do prêmio que as vendas daqui mostram: dupla fonte discorda.
// shortcut: "metade" e o piso de 15 % são hipóteses; o caso que motivou é grupo medido a 1,00× lá e 2×+ aqui.
pub fn diverges(ours: f64, external: f64) -> bool {
    ours > 1.15 && external - 1.0 < (ours - 1.0) / 2.0
}

/// Tendência de 30 dias do grupo: média geométrica das vendas dos últimos 15 dias ÷ a dos 15 anteriores, só
/// queda (alta não credita), mínimo de 3 vendas em cada janela. `(razão, venda)` com `sold_at` em segundos.
pub fn trend(sales: &[(f64, i64)], now: i64) -> f64 {
    const HALF: i64 = 15 * 86_400;
    let geo = |lo: i64, hi: i64| -> Option<f64> {
        let v: Vec<f64> = sales
            .iter()
            .filter(|(r, t)| *t > lo && *t <= hi && *r > 0.0)
            .map(|(r, _)| r.ln())
            .collect();
        (v.len() >= 3).then(|| (v.iter().sum::<f64>() / v.len() as f64).exp())
    };
    match (geo(now - HALF, now), geo(now - 2 * HALF, now - HALF)) {
        (Some(recent), Some(old)) => (recent / old).clamp(TREND_FLOOR, 1.0),
        _ => 1.0,
    }
}

/// Encolhimento da skin: se o seed pesa de verdade no preço dela (teste de permutação sobre o log das razões,
/// só seeds com 2+ vendas), `k` cai para `variância dentro ÷ variância entre seeds` (mínimo `K_MIN`); senão `K`.
/// É o "catálogo gerado dos dados": a skin sem tabela entra quando o seed dela paga de forma estatisticamente
/// relevante.
pub fn seed_k(logs: &[(u16, f64)]) -> f64 {
    let mut by: BTreeMap<u16, Vec<f64>> = BTreeMap::new();
    for (s, x) in logs.iter().filter(|(_, x)| x.is_finite()) {
        by.entry(*s).or_default().push(*x);
    }
    let groups: Vec<Vec<f64>> = by.into_values().filter(|g| g.len() >= 2).collect();
    let (g, n) = (groups.len(), groups.iter().map(Vec::len).sum::<usize>());
    if g < MIN_REPEATED_SEEDS {
        return K;
    }
    let lens: Vec<usize> = groups.iter().map(Vec::len).collect();
    let mut flat: Vec<f64> = groups.concat();
    let mean = flat.iter().sum::<f64>() / n as f64;
    let total: f64 = flat.iter().map(|x| (x - mean).powi(2)).sum();
    let between = |v: &[f64]| -> f64 {
        let mut at = 0;
        lens.iter()
            .map(|&l| {
                let m = v[at..at + l].iter().sum::<f64>() / l as f64;
                at += l;
                l as f64 * (m - mean).powi(2)
            })
            .sum()
    };
    let observed = between(&flat);
    // xorshift fixo: o mesmo banco dá o mesmo K
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut at_least = 1;
    for _ in 0..PERMUTATIONS {
        for i in (1..n).rev() {
            flat.swap(i, (next() % (i as u64 + 1)) as usize);
        }
        at_least += usize::from(between(&flat) >= observed);
    }
    if at_least as f64 / (PERMUTATIONS + 1) as f64 >= SIGNIFICANT {
        return K;
    }
    let within = ((total - observed) / (n - g) as f64).max(1e-6);
    let n0 =
        (n as f64 - lens.iter().map(|l| (l * l) as f64).sum::<f64>() / n as f64) / (g - 1) as f64;
    let tau2 = (observed / (g - 1) as f64 - within) / n0;
    if tau2 <= 0.0 {
        K
    } else {
        (within / tau2).clamp(K_MIN, K)
    }
}

/// Teto de exploração: o que já está aberto em compra de exploração (`open_explore`) mais este anúncio cabe em
/// `pct` % do capital (saldo + tudo que está comprado e não vendido)? Duro: `pct` 0 desliga a exploração.
pub fn explore_allows(price: i64, open_explore: i64, open_all: i64, saldo: i64, pct: i64) -> bool {
    let capital = saldo.max(0).saturating_add(open_all.max(0));
    let cap = capital.saturating_mul(pct.clamp(0, 100)) / 100;
    price > 0 && open_explore.saturating_add(price) <= cap
}

/// Maior preço que o prêmio provado aguenta: o saldo vezes quanto do prêmio esperado sobra no limite inferior
/// (0 sem prêmio, 1 com prova de sobra). Tamanho proporcional à confiança.
pub fn stake_cap(saldo: i64, t: &[f64; 3]) -> i64 {
    let share = if t[1] > 1.0 {
        ((t[0] - 1.0) / (t[1] - 1.0)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (saldo.max(0) as f64 * share).floor() as i64
}

/// Horas até vender na fila do seed: `ahead` anúncios dele já à venda por esse preço ou menos, e o giro do
/// nome × a parte dele que é desse seed. `None` sem giro medido.
pub fn queue_eta_hours(ahead: usize, name_sales_per_day: f64, seed_share: f64) -> Option<f64> {
    let rate = name_sales_per_day * seed_share;
    (rate.is_finite() && rate > 0.0).then(|| (ahead as f64 + 1.0) / rate * 24.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev<'a>(own: &'a [f64], group: &'a [f64], skin: &'a [f64]) -> Evidence<'a> {
        Evidence {
            own,
            group,
            skin,
            external: None,
        }
    }

    #[test]
    fn less_proof_means_a_smaller_assumed_premium() {
        let sales = |n: usize| vec![3.0; n];
        let mut last = Posterior {
            mean: 1.0,
            lower: 0.0,
            upper: 1.0,
        };
        for n in [0, 1, 3, 10, 40, 200] {
            let p = posterior(&ev(&sales(n), &[], &[]), K);
            assert!(p.lower <= p.mean && p.mean <= p.upper, "{p:?}");
            assert!(p.mean >= last.mean && p.lower >= last.lower, "{n}: {p:?}");
            last = p;
        }
        // com muita prova o encolhimento some: a média chega perto de 3× e o limite fica logo abaixo
        assert!(
            (last.mean - 3.0).abs() < 0.1 && last.lower > 2.6,
            "{last:?}"
        );
        // sem prova nenhuma: sem prêmio
        let none = posterior(&ev(&[], &[], &[]), K);
        assert!((none.mean - 1.0).abs() < 1e-9 && none.lower < 1.0);
    }

    #[test]
    fn own_sales_beat_the_group_and_the_group_beats_the_skin() {
        let (skin, group) = (vec![1.0; 50], vec![2.0; 8]);
        let g = posterior(&ev(&[], &group, &skin), K).mean;
        let s = posterior(&ev(&[], &[], &skin), K).mean;
        assert!(g > s && (s - 1.0).abs() < 1e-9, "{g} {s}");
        // o seed com 4 vendas a 4× puxa acima do grupo, sem chegar a 4×
        let o = posterior(&ev(&[4.0; 4], &group, &skin), K).mean;
        assert!(o > g && o < 4.0, "{o}");
        // sem grupo, as mesmas vendas no próprio nível dão o mesmo esperado (nenhuma conta duas vezes)
        assert!((posterior(&ev(&[2.0; 8], &[], &skin), K).mean - g).abs() < 1e-9);
        // lixo não entra
        let dirty = posterior(&ev(&[f64::NAN, 0.0, -3.0], &[], &[]), K);
        assert!((dirty.mean - 1.0).abs() < 1e-9);
    }

    #[test]
    fn external_table_is_one_more_prior_and_the_divergence_veto_reads_it() {
        let skin = vec![1.0; 20];
        let base = posterior(&ev(&[], &[], &skin), K).mean;
        let with = |x| {
            posterior(
                &Evidence {
                    skin: &skin,
                    external: Some(x),
                    ..Default::default()
                },
                K,
            )
            .mean
        };
        assert!(with(3.0) > base && with(0.5) < base);
        assert!((with(1.0) - base).abs() < 0.05);
        // nosso 2× contra 1,00× lá: discorda; 2× contra 1,8× lá: não
        assert!(diverges(2.0, 1.0) && !diverges(2.0, 1.8));
        assert!(!diverges(1.1, 1.0), "sem prêmio daqui, nada a vetar");
    }

    #[test]
    fn exit_cut_cuts_the_premium_and_never_the_common_price() {
        assert!((realistic(2.0, 1.0) - 1.85).abs() < 1e-9);
        assert_eq!(realistic(0.9, 1.0), 0.9);
        assert_eq!(realistic(1.0, 0.5), 1.0);
        // grupo caindo corta mais, com piso
        assert!(realistic(2.0, 0.8) < realistic(2.0, 1.0));
        assert_eq!(realistic(2.0, 0.1), realistic(2.0, TREND_FLOOR));
        let p = Posterior {
            mean: 3.0,
            lower: 1.5,
            upper: 4.0,
        };
        assert_eq!(
            triple(&p, 1.0),
            [1.425, 2.7, 3.55].map(|x| (x * 1e9_f64).round() / 1e9)
        );
    }

    #[test]
    fn margin_threshold_comes_from_the_steam_fee() {
        assert!(!clears_margin(1.0) && !clears_margin(1.2));
        assert!(clears_margin(1.3) && clears_margin(5.0));
        assert!(!clears_margin(f64::NAN) && !clears_margin(f64::INFINITY));
    }

    #[test]
    fn trend_only_counts_a_fall_and_needs_three_sales_per_window() {
        const D: i64 = 86_400;
        let now = 100 * D;
        let at = |r: f64, days_ago: i64| (r, now - days_ago * D);
        let falling: Vec<_> = [
            at(2.0, 20),
            at(2.0, 22),
            at(2.0, 25),
            at(1.5, 2),
            at(1.5, 3),
            at(1.5, 5),
        ]
        .to_vec();
        assert!((trend(&falling, now) - 0.75).abs() < 1e-9);
        let rising: Vec<_> = [
            at(1.5, 20),
            at(1.5, 22),
            at(1.5, 25),
            at(2.0, 2),
            at(2.0, 3),
            at(2.0, 5),
        ]
        .to_vec();
        assert_eq!(trend(&rising, now), 1.0);
        assert_eq!(
            trend(&falling[..5], now),
            1.0,
            "janela recente com 2 vendas"
        );
        assert_eq!(
            trend(
                &[
                    at(0.1, 20),
                    at(0.1, 22),
                    at(0.1, 25),
                    at(2.0, 2),
                    at(2.0, 3),
                    at(2.0, 5)
                ],
                now
            ),
            1.0
        );
        assert_eq!(
            trend(
                &[
                    at(1.0, 20),
                    at(1.0, 22),
                    at(1.0, 25),
                    at(0.1, 2),
                    at(0.1, 3),
                    at(0.1, 5)
                ],
                now
            ),
            TREND_FLOOR
        );
    }

    /// Skin onde o seed pesa: 12 seeds com 4 vendas cada, vendas de um seed parecidas entre si.
    fn skin_with_seed_effect() -> Vec<(u16, f64)> {
        (0..12u16)
            .flat_map(|s| (0..4).map(move |i| (s, f64::from(s) * 0.4 + f64::from(i) * 0.02)))
            .collect()
    }

    #[test]
    fn seed_effect_lowers_shrinkage_only_when_it_is_significant() {
        let strong = seed_k(&skin_with_seed_effect());
        assert!((K_MIN..K).contains(&strong), "{strong}");
        // mesmo ruído para todo seed: sem efeito, K padrão
        let noise: Vec<(u16, f64)> = (0..12u16)
            .flat_map(|s| {
                (0..4).map(move |i| (s, f64::from((u32::from(s) * 7 + i * 13) % 5) * 0.1))
            })
            .collect();
        // permutar pouca coisa não é significativo
        let k = seed_k(&noise);
        assert!(k > strong, "{k} {strong}");
        // poucos seeds repetidos: não há teste
        assert_eq!(seed_k(&skin_with_seed_effect()[..12]), K);
        assert_eq!(seed_k(&[]), K);
        // determinístico
        assert_eq!(seed_k(&skin_with_seed_effect()), strong);
    }

    #[test]
    fn exploration_cap_counts_the_open_exploration_against_the_whole_capital() {
        // capital R$ 36,00 (saldo 30 + 6 abertos), teto 12 % = R$ 4,32
        assert!(explore_allows(300, 0, 600, 3000, 12));
        assert!(explore_allows(432, 0, 600, 3000, 12));
        assert!(!explore_allows(433, 0, 600, 3000, 12));
        // o que já está aberto em exploração gasta o teto
        assert!(explore_allows(100, 300, 600, 3000, 12));
        assert!(!explore_allows(150, 300, 600, 3000, 12));
        // 0 % desliga; > 100 % vale 100 %; preço inválido nunca
        assert!(!explore_allows(1, 0, 0, 10_000, 0));
        assert!(explore_allows(10_000, 0, 0, 10_000, 500));
        assert!(!explore_allows(0, 0, 0, 10_000, 12) && !explore_allows(-5, 0, 0, 10_000, 12));
        assert!(!explore_allows(i64::MAX, i64::MAX, i64::MAX, i64::MAX, 12));
    }

    #[test]
    fn stake_grows_with_the_share_of_the_premium_the_lower_bound_keeps() {
        assert_eq!(stake_cap(3600, &[2.0, 2.0, 2.5]), 3600);
        assert_eq!(stake_cap(3600, &[1.5, 2.0, 2.5]), 1800);
        assert_eq!(stake_cap(3600, &[0.9, 2.0, 2.5]), 0);
        assert_eq!(stake_cap(3600, &[1.0, 1.0, 1.0]), 0);
        assert_eq!(stake_cap(-5, &[2.0, 2.0, 2.0]), 0);
    }

    #[test]
    fn queue_time_counts_the_listings_ahead_and_needs_a_measured_rate() {
        // 10 vendas/dia do nome, 1/5 delas desse seed: 2/dia; sozinho na fila leva 12 h, com 3 na frente 48 h
        assert_eq!(queue_eta_hours(0, 10.0, 0.2), Some(12.0));
        assert_eq!(queue_eta_hours(3, 10.0, 0.2), Some(48.0));
        assert_eq!(queue_eta_hours(0, 10.0, 0.0), None);
        assert_eq!(queue_eta_hours(0, f64::NAN, 0.2), None);
    }
}
