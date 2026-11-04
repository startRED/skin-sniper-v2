//! Melhor combinação de anúncios que cabe no saldo: mochila 0/1 em centavos, maximiza o lucro.
//!
//! Exata (programação dinâmica) para saldo ≤ `EXACT_BALANCE_CENTS` e até `MAX_ITEMS` itens. Fora disso
//! vale o atalho documentado em `best_combo`.

pub const EXACT_BALANCE_CENTS: i64 = 20_000; // R$ 200
pub const MAX_ITEMS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub name: String,
    pub cost_cents: i64,
    pub profit_cents: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Combo {
    pub ids: Vec<String>,
    pub cost_cents: i64,
    pub profit_cents: i64,
}

/// Escolhe o subconjunto de maior lucro cujo custo total não passa de `balance_cents`.
/// Ignora itens com custo ≤ 0, lucro ≤ 0 ou custo acima do saldo. Se o ótimo for uma única skin,
/// prefere duas ou três skins de nomes distintos quando rendem ao menos 90% do lucro ótimo.
///
/// shortcut: acima de R$ 200 os custos são arredondados PARA CIMA em passos de `ceil(saldo / 20000)`
/// centavos (nunca estoura o saldo, mas pode perder um pouco de lucro: no máximo um passo por item
/// escolhido); acima de 200 itens só entram os 200 de maior lucro/custo. Subir o limite se essa perda
/// aparecer em dados reais.
pub fn best_combo(items: &[Candidate], balance_cents: i64) -> Combo {
    let mut cands: Vec<&Candidate> = items
        .iter()
        .filter(|c| c.cost_cents > 0 && c.profit_cents > 0 && c.cost_cents <= balance_cents)
        .collect();
    if cands.is_empty() {
        return Combo::default();
    }
    if cands.len() > MAX_ITEMS {
        // lucro/custo decrescente, sem f64: a/b > c/d ⇔ a·d > c·b
        cands.sort_by(|a, b| (b.profit_cents * a.cost_cents).cmp(&(a.profit_cents * b.cost_cents)));
        cands.truncate(MAX_ITEMS);
    }
    let step = ((balance_cents + EXACT_BALANCE_CENTS - 1) / EXACT_BALANCE_CENTS).max(1);
    let cap = (balance_cents / step) as usize;
    let w: Vec<usize> = cands
        .iter()
        .map(|c| ((c.cost_cents + step - 1) / step) as usize)
        .collect();

    // best[c] = maior lucro com custo (arredondado) ≤ c; taken[i][c] guarda a decisão para reconstruir.
    let mut best = vec![0i64; cap + 1];
    let mut taken = vec![false; cands.len() * (cap + 1)];
    for (i, c) in cands.iter().enumerate() {
        let wi = w[i];
        for cc in (wi..=cap).rev() {
            let with = best[cc - wi] + c.profit_cents;
            if with > best[cc] {
                best[cc] = with;
                taken[i * (cap + 1) + cc] = true;
            }
        }
    }
    let mut out = Combo::default();
    let mut cc = cap;
    for i in (0..cands.len()).rev() {
        if taken[i * (cap + 1) + cc] {
            out.ids.push(cands[i].id.clone());
            out.cost_cents += cands[i].cost_cents;
            out.profit_cents += cands[i].profit_cents;
            cc -= w[i];
        }
    }
    out.ids.reverse();
    if out.ids.len() == 1 {
        let mut diversified: Option<Combo> = None;
        for i in 0..cands.len() {
            for j in i + 1..cands.len() {
                if cands[i].name == cands[j].name {
                    continue;
                }
                for third in std::iter::once(None).chain(cands[j + 1..].iter().copied().map(Some)) {
                    if third.is_some_and(|c| c.name == cands[i].name || c.name == cands[j].name) {
                        continue;
                    }
                    let cost = i128::from(cands[i].cost_cents)
                        + i128::from(cands[j].cost_cents)
                        + third.map_or(0, |c| i128::from(c.cost_cents));
                    let profit = i128::from(cands[i].profit_cents)
                        + i128::from(cands[j].profit_cents)
                        + third.map_or(0, |c| i128::from(c.profit_cents));
                    if cost <= i128::from(balance_cents)
                        && profit <= i128::from(i64::MAX)
                        && profit * 10 >= i128::from(out.profit_cents) * 9
                        && diversified
                            .as_ref()
                            .is_none_or(|best| profit > i128::from(best.profit_cents))
                    {
                        let mut ids = vec![cands[i].id.clone(), cands[j].id.clone()];
                        if let Some(c) = third {
                            ids.push(c.id.clone());
                        }
                        diversified = Some(Combo {
                            ids,
                            cost_cents: cost as i64,
                            profit_cents: profit as i64,
                        });
                    }
                }
            }
        }
        if let Some(combo) = diversified {
            return combo;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn c(id: &str, cost: i64, profit: i64) -> Candidate {
        Candidate {
            id: id.into(),
            name: id.into(),
            cost_cents: cost,
            profit_cents: profit,
        }
    }

    #[test]
    fn empty_and_unaffordable() {
        assert_eq!(best_combo(&[], 1000), Combo::default());
        assert_eq!(best_combo(&[c("a", 500, 50)], 499), Combo::default());
        assert_eq!(best_combo(&[c("a", 500, 50)], 0), Combo::default());
        assert_eq!(best_combo(&[c("a", 500, 50)], -10), Combo::default());
    }

    #[test]
    fn ignores_nonpositive_profit_and_cost() {
        let got = best_combo(
            &[
                c("loss", 100, -5),
                c("zero", 100, 0),
                c("free", 0, 10),
                c("ok", 100, 7),
            ],
            1000,
        );
        assert_eq!(got.ids, ["ok"]);
    }

    #[test]
    fn single_item_exactly_the_balance() {
        let got = best_combo(&[c("a", 500, 50)], 500);
        assert_eq!(
            (got.ids, got.cost_cents, got.profit_cents),
            (vec!["a".into()], 500, 50)
        );
    }

    #[test]
    fn beats_greedy_by_ratio() {
        // guloso por razão pega "a" (razão 0,6) e perde: a+b não cabe; b+c rende mais
        let items = [c("a", 60, 36), c("b", 50, 28), c("c", 50, 28)];
        let got = best_combo(&items, 100);
        assert_eq!(got.profit_cents, 56);
        assert_eq!(got.ids, ["b", "c"]);
    }

    #[test]
    fn matches_brute_force_on_small_cases() {
        // gerador determinístico
        let mut x: u64 = 12345;
        let mut next = |m: u64| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) % m
        };
        for _ in 0..50 {
            let n = 1 + next(10) as usize;
            let items: Vec<Candidate> = (0..n)
                .map(|i| c(&i.to_string(), 1 + next(300) as i64, 1 + next(80) as i64))
                .collect();
            let bal = 100 + next(800) as i64;
            let mut want = 0;
            for mask in 0u32..(1 << n) {
                let (mut cost, mut profit) = (0, 0);
                for (i, it) in items.iter().enumerate() {
                    if mask >> i & 1 == 1 {
                        cost += it.cost_cents;
                        profit += it.profit_cents;
                    }
                }
                if cost <= bal {
                    want = want.max(profit);
                }
            }
            let got = best_combo(&items, bal);
            assert!(got.profit_cents * 10 >= want * 9);
            assert!(got.cost_cents <= bal);
        }
    }

    #[test]
    fn favors_distinct_skins_only_within_ten_percent() {
        let single = c("single", 100, 100);
        let a = c("a", 50, 45);
        let b = c("b", 50, 45);
        assert_eq!(
            best_combo(&[single.clone(), a.clone(), b.clone()], 100).ids,
            ["a", "b"]
        );
        let mut lower = b.clone();
        lower.profit_cents = 44;
        assert_eq!(
            best_combo(&[single.clone(), a.clone(), lower], 100).ids,
            ["single"]
        );
        let mut same = b;
        same.name = a.name.clone();
        assert_eq!(best_combo(&[single, a, same], 100).ids, ["single"]);
    }

    #[test]
    fn favors_three_distinct_skins() {
        let got = best_combo(
            &[
                c("single", 90, 100),
                c("a", 30, 30),
                c("b", 30, 30),
                c("c", 30, 30),
            ],
            90,
        );
        assert_eq!(got.ids, ["a", "b", "c"]);
    }

    #[test]
    fn exact_case_is_fast() {
        // pior caso do contrato: R$ 200 e 200 itens
        let items: Vec<Candidate> = (0..200)
            .map(|i| c(&i.to_string(), 150 + (i * 37 % 900), 5 + (i * 13 % 120)))
            .collect();
        let t = Instant::now();
        let got = best_combo(&items, 20_000);
        let ms = t.elapsed().as_millis();
        assert!(got.cost_cents <= 20_000 && got.profit_cents > 0);
        // o contrato (< 50 ms) vale para o binário de release; em debug o laço é ~10× mais lento
        let limit = if cfg!(debug_assertions) { 500 } else { 50 };
        assert!(ms < limit, "levou {ms} ms");
    }

    #[test]
    fn large_balance_never_overspends() {
        let items: Vec<Candidate> = (0..300)
            .map(|i| c(&i.to_string(), 1000 + (i * 977 % 9000), 50 + (i * 31 % 700)))
            .collect();
        let got = best_combo(&items, 500_000);
        assert!(got.cost_cents <= 500_000 && got.ids.len() <= MAX_ITEMS);
        let sum: i64 = got
            .ids
            .iter()
            .map(|id| items[id.parse::<usize>().unwrap()].cost_cents)
            .sum();
        assert_eq!(sum, got.cost_cents);
    }
}
