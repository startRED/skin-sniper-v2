# Aprendizado da caça — plano de implementação

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a agenda passa a aprender com cada anúncio novo visto (nota por proximidade, com escala medida pelo
próprio v2), e a seleção de descontos troca os alvos que rendem pouco por skins ainda não testadas.

**Architecture:** um módulo novo `src/hunt/learn.rs` com as duas contas puras (crédito por proximidade, escala da
cauda) e a leitura da escala na tabela `evaluations`. `detect::process` usa o crédito; `Queue::boosts` em
`scheduler.rs` dá o fator aprendido que `catalog::select` usa no lugar de `(1 + acertos)`.

**Tech Stack:** Rust estável, tokio, rusqlite (já no `Cargo.toml`). Nenhuma crate nova.

**Spec:** `docs/superpowers/specs/2026-11-06-aprendizado-design.md`

## Global Constraints

- Nenhuma crate nova (AGENTS regra 5). Dinheiro em centavos `i64`; `f64` só na nota da agenda, nunca em preço.
- Código em inglês; comentários, mensagens de log e docs em português (Brasil).
- Testes sem rede (servidor local de `hunt/tests.rs` serve os fixtures).
- A régua de dinheiro não muda: `sell::evaluate`, `MIN_LIST_PROFIT_CENTS`, `MIN_LIST_MARGIN_PCT` intactos.
- Números de calibração (hipóteses, escritos assim no `projeto.md`): amostra 5 000, mínimo 200, τ ∈ [0,01; 0,10],
  τ padrão 0,03, quantil 0,9, encolhimento 20 consultas, fator ∈ [0,25; 4], referência com ≥ 10 consultas.
- Prova de pronto: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.

## Review Focus

1. Banco novo, `evaluations` vazia: τ fica em 0,03 e nada quebra (teste em Task 2).
2. Preço de 1 centavo ou lucro no médio muito acima da régua: crédito finito, igual ao lucro mínimo (Task 1).
3. Todas as notas antigas com `r = 0` (instalação sobre `arm_stats` velho): `boosts` vazio, seleção igual à de
   hoje (Task 3).
4. Alvo que saiu da seleção há dias: o fator volta para perto de 1, e ele pode voltar (Task 3).
5. O mesmo anúncio reavaliado (histórico mudou) não rende crédito de novo (Task 1).

---

### Task 1: Crédito por proximidade em cada anúncio novo

**Files:**
- Create: `src/hunt/learn.rs`
- Modify: `src/hunt/mod.rs` (declarar `mod learn;`; campo `closeness_scale: f64` em `Shared`, iniciado com
  `learn::DEFAULT_SCALE`)
- Modify: `src/hunt/detect.rs:6,157-162,689-699`
- Modify: `src/hunt/scheduler.rs:39-40,305-306,391` (remover `NEAR_WEIGHT`, atualizar os comentários de `Found`)
- Test: `src/hunt/learn.rs` (módulo `tests`), `src/hunt/tests.rs`

**Interfaces:**
- Produces: `pub(crate) const DEFAULT_SCALE: f64 = 0.03;`
  `pub(crate) fn closeness_credit(price_cents: i64, mid_profit_cents: i64, scale: f64) -> f64`
  e o campo `Shared::closeness_scale: f64`.

- [ ] **Step 1: testes unitários em `learn.rs`**

```rust
#[test]
fn closeness_is_continuous_and_falls_by_e_per_scale() {
    // R$ 20,00: lucro mínimo = R$ 2,00 (10 %); faltou 0 → 200; faltou 3 pontos (60 c) → 200/e
    assert_eq!(closeness_credit(2000, 200, 0.03), 200.0);
    assert_eq!(closeness_credit(2000, 900, 0.03), 200.0); // passou da régua: teto no lucro mínimo
    let one_scale = closeness_credit(2000, 140, 0.03);
    assert!((one_scale - 200.0 / std::f64::consts::E).abs() < 1e-9, "{one_scale}");
    assert!(closeness_credit(2000, -500, 0.03) < 0.01);
    let tiny = closeness_credit(1, 0, 0.03); // preço de 1 centavo: finito
    assert!(tiny.is_finite() && tiny >= 0.0);
}
```

- [ ] **Step 2: integração em `hunt/tests.rs`**: `new_listings_teach_the_agenda_once`. Monte como
  `hourly_listings_include_above_balance_without_counting_repeats` (mesma `Task` `d:x`, `MOON`, página de
  `listings_body()`), mas com `h.set_saldo(3608, SaldoSource::Manual)`. Rode `detect::process` duas vezes na mesma
  página:

```rust
let first = detect::process(&h, &task, &page).await.unwrap();
assert!(first.rewarded.near_credit > 0.0, "{:?}", first.rewarded);
let again = detect::process(&h, &task, &page).await.unwrap();
assert_eq!(again.rewarded.near_credit, 0.0);
assert_eq!(again.rewarded.finds, 0);
```

- [ ] **Step 3: rodar e ver falhar** — `cargo test closeness new_listings_teach` → erro de compilação
  (`learn` não existe).

- [ ] **Step 4: implementar.** `closeness_credit`: `need = catalog::min_profit(price)`,
  `g = (mid − need) as f64 / price.max(1) as f64`, devolve `need as f64 * (g.min(0.0) / scale).exp()`. Em
  `detect::process`, para cada avaliação sem `found` com `near`, some `closeness_credit(n.price_cents,
  n.mid_cents, scale)` em `out.rewarded.near_credit` **só quando `!was_known`**. `scale` vem de
  `h.lock().closeness_scale`, lido uma vez antes do laço. O contador `near_hits` segue a janela de hoje
  (`short > 0 && short <= 30.max(price / 10)`): `near_credit` vira `fn is_near(n: &Near) -> bool`. Remova
  `NEAR_WEIGHT`, que deixa de ser usada.

- [ ] **Step 5: rodar** — `cargo test` → tudo verde, inclusive `near_misses_give_a_new_arm_a_head_start`, que monta
  `Found` à mão.

- [ ] **Step 6: commit** — `git add src/hunt/learn.rs src/hunt/mod.rs src/hunt/detect.rs src/hunt/scheduler.rs src/hunt/tests.rs`
  e `git commit -m "Agenda: cada anúncio novo rende pela proximidade de dar lucro"`.

### Task 2: Escala τ medida nas avaliações

**Files:**
- Modify: `src/hunt/learn.rs`, `src/hunt/catalog.rs` (`reselect_now` chama `learn::refresh_scale`)
- Test: `src/hunt/learn.rs`, `src/hunt/tests.rs`

**Interfaces:**
- Consumes: `DEFAULT_SCALE`, `Shared::closeness_scale` (Task 1).
- Produces: `pub(crate) fn tail_scale(gaps: &mut [f64]) -> Option<f64>` e
  `pub(crate) async fn refresh_scale(h: &Hunt) -> anyhow::Result<()>`.

- [ ] **Step 1: testes unitários**

```rust
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
```

- [ ] **Step 2: integração** `closeness_scale_defaults_then_learns_from_evaluations` em `hunt/tests.rs`: com o
  banco novo (`temp_db`) e `refresh_scale(&h)`, `h.lock().closeness_scale == learn::DEFAULT_SCALE`. Insira 300 linhas
  em `evaluations` com `price_cents = 1000` e `mid_profit_cents = -k` (k = 0..300). As colunas NOT NULL `at`,
  `listing_id`, `name`, `steam_url` e `reason` recebem valores quaisquer. Chame `refresh_scale` de novo; a
  escala deve mudar para o valor de `tail_scale` dos mesmos gaps, calculado no teste.

- [ ] **Step 3: rodar e ver falhar** — `cargo test tail_scale closeness_scale_defaults` → não compila.

- [ ] **Step 4: implementar.** `tail_scale`: menos de `SCALE_MIN_ROWS = 200` → `None`; ordena com `total_cmp`;
  `x0 = gaps[(0.9 * (n − 1) as f64).floor() as usize]`; τ = média de `g − x0` para `g ≥ x0`; `clamp(0.01, 0.10)`.
  `refresh_scale`: `SELECT price_cents, mid_profit_cents FROM evaluations WHERE mid_profit_cents IS NOT NULL
  ORDER BY id DESC LIMIT 5000`; g = `(mid − min_profit(price)) / price.max(1)`; τ = `tail_scale` ou
  `DEFAULT_SCALE`. Se mudou mais de 1 %, grave em `closeness_scale` e logue
  `tracing::info!(escala = τ, avaliacoes = n, "escala de proximidade da agenda")`. Em `reselect_now`, chame
  depois de `load_items`. Uma falha só loga `warn` e segue a seleção, com a escala anterior.

- [ ] **Step 5: rodar** — `cargo test` → verde.

- [ ] **Step 6: commit** — `git add src/hunt/learn.rs src/hunt/catalog.rs src/hunt/tests.rs` e
  `git commit -m "Agenda: escala da proximidade medida nas últimas avaliações"`.

### Task 3: Fator aprendido na seleção (o catálogo gira)

**Files:**
- Modify: `src/hunt/scheduler.rs` (novo `Queue::boosts`), `src/hunt/catalog.rs:229-260,812-831,876-888,994-1006,1350-1365`
- Test: `src/hunt/scheduler.rs`, `src/hunt/catalog.rs`

**Interfaces:**
- Produces: `pub fn boosts(&self, now: i64) -> HashMap<String, f64>` em `Queue`; `Ctx::boost: &HashMap<String, f64>`
  no lugar de `Ctx::hits`.

- [ ] **Step 1: testes em `scheduler.rs`** `boosts_reward_yield_and_fade_back_to_one`: monte `Queue` e chame
  `load_stats` com `Stat`s escritos à mão (`updated_at = 0`):
  - `d:good`: `n = 100`, `r = 1000`, `queries = 100`;
  - `d:mid1`, `d:mid2`: `n = 100`, `r = 100`, `queries = 100`;
  - `d:bad`: `n = 100`, `r = 50`, `queries = 100` (encolhido: 70/120 ≈ 0,58);
  - `d:new`: `queries = 5`, `n = 5`, `r = 0` (fica fora da referência por ter menos de 10 consultas);
  - `r:rare`: `n = 100`, `r = 5000`, `queries = 100` (só `d:` entra).

  A referência é a mediana de `r/n` entre `good`, `mid1`, `mid2` e `bad` (número par: média dos dois do meio),
  ou seja 1,0. Em `now = 0`:
  `b["d:good"] == 4.0` (teto), `b["d:bad"]` em `(0.25, 1.0)`, `(b["d:mid1"] - 1.0).abs() < 1e-9`, `b` não tem
  `r:rare`. Em `now = 10 × 86 400` (10 meias-vidas): `(b["d:good"] - 1.0).abs() < 0.2` e
  `(b["d:bad"] - 1.0).abs() < 0.2`. Com todas as `r = 0`, `boosts` sai vazio.

- [ ] **Step 2: teste em `catalog.rs`**: troque `NO_HITS` e `hits` por `boost: HashMap<String, f64>`. Em
  `hits` → `boost = {"d:S0 (Field-Tested)": 4.0}`, `tasks[0].id` continua `"d:S0 (Field-Tested)"`. Acrescente que
  `boost = {"d:S99 (Field-Tested)": 0.25}` tira `S99` do topo.

- [ ] **Step 3: rodar e ver falhar** — `cargo test boosts select` → não compila.

- [ ] **Step 4: implementar.** `boosts`: para cada stat `d:` com `queries > 0`, decai `n` e `r` até `now` sem
  mutar (`0.5^((now − updated_at)/HALF_LIFE)`). Referência = mediana de `r/n` dos `d:` com `queries ≥ NEW_QUERIES`
  e `n > 0`. Com referência ≤ 0 ou sem candidatos, devolve vazio. Fator = `((r + 20·ref)/(n + 20)) / ref`
  limitado a `[0.25, 4.0]`. Constantes: `BOOST_PRIOR_QUERIES = 20.0`, `BOOST_RANGE = (0.25, 4.0)`. Em `select`:
  `base * c.boost.get(&id).copied().unwrap_or(1.0)`. Em `reselect_now`:
  `let boost = h.lock().queue.boosts(now);`. Apague `recent_hits` (fica sem uso).

- [ ] **Step 5: rodar** — `cargo test` → verde; `cargo clippy --all-targets -- -D warnings` → limpo.

- [ ] **Step 6: commit** — `git add src/hunt/scheduler.rs src/hunt/catalog.rs` e
  `git commit -m "Seleção: fator aprendido troca alvo fraco por skin não testada"`.

### Task 4: Documentação e entrega

**Files:**
- Modify: `docs/projeto.md` (seção Caça, "Agenda que aprende" e "Seleção"; Precificação: uma frase com a
  conferência da previsão)
- Add: `docs/superpowers/specs/2026-11-06-aprendizado-design.md`, `docs/superpowers/plans/2026-11-06-aprendizado.md`

- [ ] **Step 1:** em "Agenda que aprende", troque "quase-lucro (… 10 % do preço …) conta 15 % do lucro mínimo"
  pela regra nova: cada anúncio novo avaliado que não passou rende `lucro mínimo × e^(g/τ)`, g = (lucro no médio
  − lucro mínimo) ÷ preço. τ = excesso médio acima do p90 das últimas 5 000 avaliações (mínimo 200, entre 1 e 10
  pontos, padrão 3 pontos medido em 4 850 anúncios de 04–06/11). Em "Seleção", troque `(1 + acertos recentes)`
  pelo fator aprendido (regra do desenho, seção 3). Marque os números como hipóteses.
- [ ] **Step 2:** em Precificação, acrescente: "Conferida em 06/11 contra o mercado 7–9 dias depois (7 525 casos,
  493 skins): real ÷ previsto no médio com mediana 1,000 (p10 0,914, p90 1,104), 50 % do volume ≥ médio e 70 % ≥
  rápido, erro médio 6,6 %."
- [ ] **Step 3:** prova de pronto completa (os três comandos das Global Constraints e `cargo build --release`).
- [ ] **Step 4: commit** — `git add docs/projeto.md docs/superpowers/specs/2026-11-06-aprendizado-design.md docs/superpowers/plans/2026-11-06-aprendizado.md`
  e `git commit -m "Docs: aprendizado da caça (desenho, plano e projeto)"`.
