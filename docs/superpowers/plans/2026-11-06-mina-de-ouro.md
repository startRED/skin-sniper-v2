# Mina de ouro — plano de implementação

> **Para agentes:** executar com superpowers:executing-plans (modo nativo, escolhido pelo Erick). Passos com
> checkbox (`- [ ]`).

**Objetivo:** o app guarda tudo o que vê, confere cada julgamento com o mercado (sem comprar) e usa esse
resultado para mirar melhor, com a prova à vista no painel.

**Arquitetura:** um registro de observações (`observations`) e um do livro (`book_snapshots`) alimentam um
professor (`hunt/teacher.rs`) que roda de hora em hora dentro do laço da caça. O professor rotula compras
virtuais (`ghosts`), guarda as vendas do CSFloat (`csfloat_sales`) e mede o prêmio que fecha por grupo raro
(`pricing/premium.rs`). A agenda de colheita (`hunt/harvest.rs`) roda em sombra e depois disputa dias alternados
com o UCB1. Tudo aparece em `GET /api/learning` e na seção "Aprendizado" do painel.

**Stack:** Rust (tokio, axum, rusqlite) + Svelte 5. Nenhum crate novo.

**Desenho:** `docs/superpowers/specs/2026-11-06-mina-de-ouro-design.md`.

## Restrições globais

- Dinheiro em centavos `i64`; datas em segundos Unix; dia do placar = dia de Brasília (UTC−3).
- Nenhum crate novo. Testes sem rede; fixtures reais de `testdata/`.
- Retenção: `observations`, `book_snapshots`, `evaluations`, `ghosts`, `csfloat_sales`: 60 dias.
- Relistagem: mesmo `asset_id` em outro `listing_id` em até 24 h.
- "Fechou" do raro: sumiço sem relistagem em até 48 h a preço ≥ o sugerido, ou venda do CSFloat a múltiplo ≥ o
  sugerido.
- Selo "comprovado": p25 do prêmio fechado, com ≥ 10 vendas na seed ou no grupo, ainda passa na régua
  (`MIN_LIST_PROFIT_CENTS` e `MIN_LIST_MARGIN_PCT`). A regra de aviso de hoje não muda.
- Carteira fantasma do comum: venda = menor entre a mediana dos dias 7–9 e o menor anúncio do livro nos dias 7–9;
  sem nenhum até o dia 12 → "sem rótulo". Não gasta o balde de anúncios.
- Colheita em sombra até 7 dias de registro; depois dias alternados (par = UCB1, ímpar = colheita) por 7 dias;
  depois vence a de mais (achados + quase-achados) por consulta.
- Recalibração: só depois de ≥ 7 dias de rótulos rápidos e ≥ 30 rótulos; só desconta (fator em [0,80; 1,00]);
  passo ≤ 2 % por dia; toda mudança em `refit_log`.
- Tráfego real da Steam só pelo limitador do app (≥ 3 s, sem rajada). Nada de escrever no banco de produção por
  fora do app.
- Commits por nome de arquivo (nunca `git add -A`); um PR por fase; CI verde antes do merge.

## Foco de revisão

1. Página vazia ou 429 no meio: nada vira "sumiu" sem página confiável (vazia nunca marca sumiço).
2. Inversão de ordem da Steam (~1 %): só some quem tem preço < 98 % do maior preço da página quando há `more`.
3. Relistagem: o anúncio antigo nunca conta como venda quando o mesmo `asset_id` volta em 24 h.
4. Crescimento do banco: retenção de 60 dias em todas as tabelas novas e livro gravado só quando o topo muda.
5. Sem estimativa ou sem dado de rótulo: a compra virtual fecha como "sem rótulo" e é contada, nunca some.

Cada linha tem teste na tarefa dona do código (abaixo).

---

## Fase 0 — registro, livro e placar (PR 1)

### Tarefa 0.1: `asset_id` no anúncio

**Arquivos:** `src/steam/listings.rs` (struct `Listing`, `parse_listing`).

- [ ] Teste `asset_id_comes_from_the_asset`: o 1º anúncio de `steam_listings_moonrise_sorted_price.json` tem
  `asset_id == "54226282359"`; JSON antigo de `Listing` sem o campo desserializa com `asset_id == ""`.
- [ ] `pub asset_id: String` com `#[serde(default)]`, lido de `asset.assetid` ou `asset.id` (string ou número).
- [ ] `cargo test steam::listings` verde.

### Tarefa 0.2: registro de observações

**Arquivos:** criar `src/hunt/ledger.rs`; `src/db.rs` (migração 7, retenção); `src/hunt/scheduler.rs`
(`query_group` chama o registro uma vez por página); `src/hunt/mod.rs` (`Shared::ledger_prev`).

**Esquema (migração 7):**

```sql
CREATE TABLE observations (
  listing_id TEXT PRIMARY KEY, asset_id TEXT, name TEXT NOT NULL, query TEXT NOT NULL, grp TEXT,
  price_cents INTEGER NOT NULL, seed INTEGER, "float" REAL, paint_index INTEGER,
  first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, age_max INTEGER, cheapest INTEGER NOT NULL,
  gone_at INTEGER, relist_of TEXT, relisted INTEGER NOT NULL DEFAULT 0);
CREATE INDEX observations_query ON observations(query, gone_at);
CREATE INDEX observations_asset ON observations(asset_id);
CREATE INDEX observations_last ON observations(last_seen);
CREATE INDEX observations_grp ON observations(grp, gone_at);
CREATE INDEX observations_name ON observations(name, gone_at);
```

**Interfaces:**
- `pub fn query_key(t: &Task) -> String` = `"{code}|{wear:?}|{filters:?}"`.
- `pub async fn record(h: &Hunt, key: &str, page: &ListingsPage, now: i64) -> Result<Recorded>`;
  `Recorded { new: u32, aged: u32, gone: u32, relisted: u32 }`.
- `Shared::ledger_prev: HashMap<String, (i64, Option<i64>)>`: última consulta da chave e o teto que ela cobria
  (`None` = página inteira, sem `more`).

**Regras:** novo → `age_max = now − prev` quando a consulta anterior cobria o preço; `cheapest` = menor preço do
mesmo nome na página. Mesmo `asset_id` com outro id ainda vivo ou sumido há ≤ 24 h → antigo `relisted = 1`
(`gone_at` preenchido se vazio) e novo `relist_of`. Reapareceu → `gone_at = NULL`. Sumiço só com página não
vazia, para quem não está nela e (sem `more` ou preço × 100 < maior × 98).

- [ ] Testes em `src/hunt/tests.rs`: `ledger_marks_gone_only_when_covered` (fora da cobertura e página vazia não
  marcam), `ledger_relist_by_asset_is_not_a_sale`, `ledger_age_bound_from_previous_cover`,
  `ledger_reappearing_listing_is_alive`.
- [ ] Implementar; retenção `last_seen < now − 60 d`; `evaluations` por tempo (60 d) em vez de 10 000 linhas.

### Tarefa 0.3: escada do livro e registro do livro

**Arquivos:** `src/steam/book.rs`; `src/hunt/orders.rs` (`remember_book`); `src/hunt/scheduler.rs`
(`book_then_listings` usa `remember_book`); `src/db.rs`.

```sql
CREATE TABLE book_snapshots (name TEXT NOT NULL, at INTEGER NOT NULL, max_buy_cents INTEGER,
  buy_orders INTEGER NOT NULL, sell_orders INTEGER NOT NULL, sell_levels TEXT NOT NULL, PRIMARY KEY (name, at));
CREATE INDEX book_snapshots_at ON book_snapshots(at);
```

- `Book` ganha `sell_orders: i64` e `sell_levels: Vec<(i64, i64)>` (até 5 degraus `[preço, quantidade]` de
  `rgCompactSellOrders`); deixa de ser `Copy`.
- `pub fn second_ask(&self) -> Option<i64>`: preço do 2º anúncio (o mesmo degrau se a quantidade ≥ 2).
- `pub(crate) async fn remember_book(h: &Hunt, name: &str, now: i64, b: Option<Book>)`: guarda em `Shared::books`
  e grava snapshot quando `(max_buy, sell_orders, sell_levels)` mudou.
- [ ] Teste `live_capture_ladder`: Negev → degraus `[(167,2),(170,1),(179,1),(189,1),(200,1)]`, `second_ask ==
  Some(167)`. Teste `book_snapshot_only_on_change` (duas leituras iguais = 1 linha).

### Tarefa 0.4: placar diário

**Arquivos:** `src/db.rs` (colunas `queries`, `near`, `books` em `hourly_activity`; retenção 60 d); criar
`src/web/learning.rs` (`GET /api/learning`); `src/web/mod.rs` (rota); `web/src/lib/Learning.svelte`;
`web/src/lib/types.ts`; `web/src/App.svelte`; `web/dev/fixture.ts`.

- `Learning { days: Vec<Day>, ... }`, `Day { day: String, queries, books, new_listings, aged_median_min:
  Option<f64>, sales, relists, life_median_min: Option<f64>, finds, near }` (14 dias).
- [ ] Teste `learning_scorecard_counts_by_brasilia_day` em `src/web/tests.rs`.

### Tarefa 0.5: sonda de `count`

- [ ] Uma leitura de anúncios com `count: 50` no `qp`, só se a leitura não estiver barrada no app de produção.
  O resultado vai para `docs/projeto.md`. Se a Steam aceitar, a leitura de raros passa a pedir mais por
  consulta (tarefa separada).

---

## Fase 1 — professor rápido (PR 2)

**Arquivos:** criar `src/hunt/teacher.rs`; `src/hunt/detect.rs` (cria a compra virtual); `src/hunt/patterns.rs` e
`detect::rare_evidence` (guardam vendas do CSFloat); `src/hunt/orders.rs` (livro pedido pelo professor);
`src/db.rs` (migração 8).

```sql
CREATE TABLE ghosts (listing_id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, grp TEXT,
  bought_at INTEGER NOT NULL, price_cents INTEGER NOT NULL, mid_price_cents INTEGER NOT NULL,
  mid_net_cents INTEGER NOT NULL, base_cents INTEGER, approved INTEGER NOT NULL,
  fast_at INTEGER, fast_share REAL, fast_median_cents INTEGER, fast_sold INTEGER, fast_closed INTEGER,
  status TEXT NOT NULL DEFAULT 'aguardando', labeled_at INTEGER, sell_cents INTEGER, net_cents INTEGER,
  profit_cents INTEGER, closed INTEGER, label_source TEXT);
CREATE INDEX ghosts_status ON ghosts(status, bought_at);
CREATE TABLE csfloat_sales (name TEXT NOT NULL, sold_at INTEGER NOT NULL, price_usd_cents INTEGER NOT NULL,
  base_usd_cents INTEGER NOT NULL, multiple REAL NOT NULL, seed INTEGER, "float" REAL, paint_index INTEGER,
  grp TEXT, PRIMARY KEY (name, sold_at, price_usd_cents));
CREATE INDEX csfloat_sales_grp ON csfloat_sales(grp, sold_at);
CREATE TABLE refit_log (at INTEGER NOT NULL, key TEXT NOT NULL, old REAL, new REAL, why TEXT NOT NULL);
```

### Tarefa 1.1: compra virtual

- Anúncio novo avaliado com estimativa e `mid_profit ≥ min_profit(preço) − max(30, preço/10)` vira `ghost`
  (`approved` = virou oportunidade). Raro guarda `grp` e `base_cents` (preço médio comum, sem prêmio).
- [ ] Teste `near_and_approved_become_ghosts` (longe da régua não vira).

### Tarefa 1.2: rótulo rápido (48 h)

- `pub(crate) fn fast_label(...)`: depois de 48 h, comum: fração das compras do histórico nas 48 h seguintes com
  mediana ≥ `mid_price` (`fast_share`), mediana ponderada (`fast_median_cents`) e sumiços sem relistagem do mesmo
  nome a ≥ `mid_price` (`fast_sold`); raro: sumiços do grupo ou vendas do CSFloat do grupo a ≥ o sugerido.
  `fast_closed = fast_share ≥ 0,5 || fast_sold ≥ 1`.
- [ ] Teste `fast_label_reads_next_48h_sales`.

### Tarefa 1.3: rótulo final

- Comum: no dia ≥ 10, `sell = min(mediana dias 7–9, menor anúncio do livro dias 7–9)`; `net = seller_gets(sell)`;
  `profit = net − preço`; dia 12 sem dado → `sem_rotulo`. Raro: dia ≥ 14, `closed` pela regra global; lucro =
  `seller_gets(mid_price)` se fechou, senão `seller_gets(mediana comum dias 7–9)`.
- O professor pede o livro (balde do livro) dos nomes com compra virtual nos dias 7–9 sem snapshot
  (`Shared::label_books`, consumido por `orders::maybe_step` antes da fila normal).
- [ ] Testes `common_label_uses_min_of_history_and_book`, `no_data_by_day_12_is_counted_as_unlabeled`,
  `rare_closed_by_group_vanish_above_suggested`.

### Tarefa 1.4: vendas do CSFloat guardadas

- `pub(crate) async fn store_csfloat(h: &Hunt, name: &str, sales: &[CsfloatSale])`: todas as vendas sem adesivo,
  com `grp` (tabela ou aprendido). Chamado pelo coletor e por `rare_evidence`.
- [ ] Teste `csfloat_sales_are_kept_with_group`.

---

## Fase 2 — prêmio que fecha e selo (PR 3)

**Arquivos:** criar `src/pricing/premium.rs`; `src/hunt/detect.rs`; `src/web/api.rs` (`Seal`); `src/notify.rs`;
`web/src/lib/OppTable.svelte`, `types.ts`, `dev/fixture.ts`.

- `pub struct Premium { pub mid: f64, pub p25: f64, pub p75: f64, pub n: usize, pub n_seed: usize }`.
- `pub fn premium(seed: Option<u16>, sales: &[(Option<u16>, f64)]) -> Premium`: média geométrica encolhida
  seed → grupo → 1× com `K = 5` vendas fictícias; p25/p75 empíricos do conjunto usado (seed com ≥ 10, senão grupo).
- `pub struct Seal { pub proven: bool, pub sales: u32, pub p25_profit_cents: Option<i64>, pub open_cents:
  Option<i64>, pub floor_cents: Option<i64>, pub text: String }` (em `web::api`, `Opportunity::seal` com
  `#[serde(default)]`).
- Vendas fechadas do grupo = `csfloat_sales` do grupo (60 d) + `observations` do grupo que sumiram em ≤ 48 h sem
  relistagem (múltiplo = preço ÷ mediana comum de 7 dias daquele nome). Parados > 48 h contam à parte.
- Preço sugerido: abrir em `base × p75`, descer até `base × mid`.
- [ ] Testes `premium_shrinks_to_group_then_neutral`, `seal_proven_needs_ten_sales_and_p25_profit`,
  `rare_opportunity_carries_seal` e `message_format` com o selo.

---

## Fase 3 — colheita em sombra (PR 4)

**Arquivos:** criar `src/hunt/harvest.rs`; `src/hunt/scheduler.rs` (`Queue` guarda `HarvestStat` e a política);
`src/db.rs` (`harvest_stats`); `src/web/learning.rs` (política do dia e comparação).

- `HarvestStat { gap: f64, reward: f64, arrivals: f64, last_at: i64, updated_at: i64 }`, meia-vida de 24 h.
- `pub fn value(st: &HarvestStat, now: i64, life: f64) -> f64` = `reward/gap · L · (1 − e^(−Δ/L))`; sem
  medida → infinito (como alvo novo no UCB1).
- `L` = mediana de vida dos anúncios que eram o mais barato e sumiram sem relistagem (registro), padrão 600 s.
- `pub enum Policy { Shadow, Ucb, Harvest }` decidido por `ledger_start` (guardado em `settings`) e pelo dia.
- A exploração continua igual; a colheita troca só a escolha fora da exploração.
- [ ] Testes `harvest_value_grows_then_saturates`, `policy_by_day_after_shadow`,
  `harvest_pick_prefers_stale_productive_target`.

---

## Fase 4 — recalibração (no PR 4)

- O professor, uma vez por dia: com ≥ 7 dias de rótulos rápidos e ≥ 30 rótulos de comum, fator = mediana de
  `fast_median_cents / mid_price_cents`, limitado a [0,80; 1,00] e a 2 % de passo por dia. Entra como `min` com a
  correção das vendas do Erick no desconto do comum. Mudança → `refit_log` e log.
- Boletim: `GET /api/learning` traz a semana atual contra a anterior (lucro virtual, achados, acerto).
- [ ] Testes `refit_waits_for_labels_and_limits_step`.

---

## Entrega de cada PR

- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`,
  `cd web && npm ci && npm run check && npm run build`.
- [ ] Commit por nome, push, `gh pr create`, CI verde, merge squash, `git pull --ff-only` em `principal`,
  `scripts/instalar.ps1 -Destino .../app`, conferir `/api/state`, `/api/learning` e o log.
- [ ] `docs/projeto.md` atualizado com o comportamento novo.
