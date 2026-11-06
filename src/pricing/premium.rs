//! Prêmio do raro que fecha e o selo "comprovado" (mina de ouro, Fase 2). Puro: as vendas fechadas chegam de fora.
//!
//! - Prêmio = média geométrica encolhida seed → grupo → 1× com `K` vendas fictícias em cada nível: poucas vendas
//!   da seed puxam para o grupo, poucas do grupo puxam para "sem prêmio".
//! - p25/p75 empíricos das vendas da seed quando ela tem ≥ `PROVEN_SALES`, senão das do grupo.
//! - Selo "comprovado": ≥ `PROVEN_SALES` vendas fechadas e o pior quartil (p25) ainda passa na régua do aviso.
//!   A regra do aviso não muda; o selo só diz o quanto confiar.
//!
//! `K = 5` e as 10 vendas são hipóteses de calibração. Docs/projeto.md, "Aprendizado".

use super::fees::seller_gets;
use super::sell::{MIN_LIST_MARGIN_PCT, MIN_LIST_PROFIT_CENTS, weighted_quantile};
use crate::steam::buy::brl;
use serde::{Deserialize, Serialize};

/// Vendas fictícias de encolhimento em cada nível.
pub const K: f64 = 5.0;
/// Vendas fechadas para o selo "comprovado".
pub const PROVEN_SALES: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub struct Premium {
    /// Múltiplo do preço comum: encolhido (`mid`) e quartis empíricos.
    pub mid: f64,
    pub p25: f64,
    pub p75: f64,
    /// Vendas do grupo e da seed.
    pub n: usize,
    pub n_seed: usize,
}

/// `sales` = `(seed, múltiplo)` das vendas fechadas do grupo.
pub fn premium(seed: Option<u16>, sales: &[(Option<u16>, f64)]) -> Premium {
    let clean: Vec<(Option<u16>, f64)> = sales
        .iter()
        .copied()
        .filter(|(_, m)| m.is_finite() && *m > 0.0)
        .collect();
    let n = clean.len();
    let group_log = clean.iter().map(|(_, m)| m.ln()).sum::<f64>() / (n as f64 + K);
    let same: Vec<f64> = clean
        .iter()
        .filter(|(s, _)| seed.is_some() && *s == seed)
        .map(|(_, m)| *m)
        .collect();
    let n_seed = same.len();
    let seed_log = (same.iter().map(|m| m.ln()).sum::<f64>() + K * group_log) / (n_seed as f64 + K);
    let used: Vec<(f64, f64)> = if n_seed >= PROVEN_SALES {
        same.iter().map(|&m| (m, 1.0)).collect()
    } else {
        clean.iter().map(|&(_, m)| (m, 1.0)).collect()
    };
    let q = |p| weighted_quantile(&used, p).unwrap_or(1.0);
    Premium {
        mid: seed_log.exp(),
        p25: q(0.25),
        p75: q(0.75),
        n,
        n_seed,
    }
}

/// Selo do raro no aviso.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Seal {
    pub proven: bool,
    /// Vendas fechadas do conjunto usado (seed com ≥ 10, senão grupo).
    pub sales: u32,
    /// Anúncios do grupo parados à venda há mais de 48 h.
    #[serde(default)]
    pub parked: u32,
    /// Lucro vendendo no pior quartil do prêmio.
    pub p25_profit_cents: Option<i64>,
    /// Preço sugerido: abrir em `base × p75` e descer até `base × prêmio encolhido`.
    pub open_cents: Option<i64>,
    pub floor_cents: Option<i64>,
    pub text: String,
}

fn num(x: f64) -> String {
    format!("{x:.1}").replace('.', ",")
}

/// `base_cents` = preço médio comum da mesma skin (sem prêmio).
pub fn seal(price_cents: i64, base_cents: Option<i64>, p: &Premium, parked: u32) -> Seal {
    let sales = if p.n_seed >= PROVEN_SALES {
        p.n_seed
    } else {
        p.n
    };
    let at = |m: f64| {
        base_cents
            .filter(|&b| b > 0)
            .map(|b| (b as f64 * m).floor() as i64)
    };
    let p25_profit = at(p.p25).map(|s| seller_gets(s) - price_cents);
    let passes = p25_profit.is_some_and(|x| {
        x >= MIN_LIST_PROFIT_CENTS
            && x as f64 * 100.0 / price_cents.max(1) as f64 >= MIN_LIST_MARGIN_PCT
    });
    let proven = sales >= PROVEN_SALES && passes;
    let mut text = match p25_profit {
        Some(x) if proven => format!(
            "comprovado: {sales} vendas fechadas, o pior quartil ainda dá {}",
            brl(x)
        ),
        _ if sales < PROVEN_SALES => {
            format!("ainda não comprovado: {sales} de {PROVEN_SALES} vendas fechadas")
        }
        Some(x) => format!(
            "ainda não comprovado: no pior quartil ({}×) dá {}",
            num(p.p25),
            brl(x)
        ),
        None => "ainda não comprovado: sem preço comum".into(),
    };
    if parked > 0 {
        let s = if parked == 1 { "" } else { "s" };
        text += &format!(" · {parked} parado{s} há mais de 48 h");
    }
    Seal {
        proven,
        sales: sales as u32,
        parked,
        p25_profit_cents: p25_profit,
        open_cents: at(p.p75.max(p.mid)),
        floor_cents: at(p.mid),
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn premium_shrinks_to_group_then_neutral() {
        // sem venda: sem prêmio
        let p = premium(Some(1), &[]);
        assert_eq!((p.mid, p.p25, p.p75, p.n, p.n_seed), (1.0, 1.0, 1.0, 0, 0));
        // 5 vendas do grupo a 2×: metade do caminho até 2× em escala log (√2)
        let g: Vec<_> = (0..5).map(|_| (None, 2.0)).collect();
        assert!(close(premium(Some(1), &g).mid, 2f64.sqrt()));
        // seed com 5 vendas a 4× num grupo de média 2×: puxa para o grupo (2^1,5); quartis do grupo
        let mut s = g.clone();
        s.extend((0..5).map(|_| (Some(7), 4.0)));
        let p = premium(Some(7), &s);
        assert!(close(p.mid, 2f64.powf(1.5)), "{p:?}");
        assert_eq!((p.p25, p.p75, p.n, p.n_seed), (2.0, 4.0, 10, 5));
        // seed com 10 vendas: quartis só dela
        s.extend((0..5).map(|_| (Some(7), 5.0)));
        let p = premium(Some(7), &s);
        assert_eq!((p.p25, p.p75, p.n_seed), (4.0, 5.0, 10));
        // muita venda: o encolhimento quase some
        let many: Vec<_> = (0..200).map(|_| (None, 2.0)).collect();
        assert!((premium(None, &many).mid - 2.0).abs() < 0.04);
        // lixo não conta
        assert_eq!(premium(None, &[(None, f64::NAN), (None, 0.0)]).n, 0);
    }

    #[test]
    fn seal_proven_needs_ten_sales_and_p25_profit() {
        let sales =
            |n: usize, m: f64| -> Vec<(Option<u16>, f64)> { (0..n).map(|_| (None, m)).collect() };
        let p = premium(None, &sales(10, 1.5));
        let s = seal(1000, Some(1000), &p, 0);
        assert!(s.proven, "{s:?}");
        assert_eq!(s.p25_profit_cents, Some(seller_gets(1500) - 1000));
        assert_eq!(s.open_cents, Some(1500));
        assert!(
            s.text.starts_with("comprovado: 10 vendas fechadas"),
            "{}",
            s.text
        );
        // 9 vendas: ainda não
        let s = seal(1000, Some(1000), &premium(None, &sales(9, 1.5)), 2);
        assert!(!s.proven);
        assert_eq!(
            s.text,
            "ainda não comprovado: 9 de 10 vendas fechadas · 2 parados há mais de 48 h"
        );
        // 10 vendas, mas o pior quartil não paga a régua
        let mut v = sales(3, 1.05);
        v.extend(sales(7, 2.0));
        let s = seal(1000, Some(1000), &premium(None, &v), 0);
        assert!(!s.proven);
        assert!(s.text.contains("pior quartil (1,1×)"), "{}", s.text);
        assert!(s.floor_cents.unwrap() < s.open_cents.unwrap());
        // sem preço comum: nunca comprovado
        let s = seal(1000, None, &p, 0);
        assert!(!s.proven && s.open_cents.is_none());
    }
}
