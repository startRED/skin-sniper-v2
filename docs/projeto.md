# Skin Sniper v2 — desenho

Fonte de fatos da Steam: `docs/conhecimento.md` (o que o projeto antigo mediu) e `docs/pesquisa-web.md`
(sondas e fontes de 03/11/2026). Este arquivo diz **o que** construir; aqueles dizem **como a Steam responde**.
Quando um fato mudar, corrija lá, não aqui.

## Objetivo

O usuário abre o painel, vê o saldo que ele digitou e uma lista viva de anúncios que dão lucro
real depois das taxas, que cabem no saldo. Ele ordena por lucro ou margem e abre o anúncio na Steam para
comprar lá. **Desde 09/11/2026 o app não usa a conta Steam** (pedido do Erick): sem login, QR, cookie de
sessão, carteira ou compra pelo servidor; só consultas públicas do mercado. O Skin
Sniper escolhe sozinho o que caçar. Leve no PC: um binário Rust, um banco SQLite que se limpa sozinho.

Não é objetivo da v1: vender pelo painel, valorizar adesivo (o chaveiro entra no lucro desde 11/11, ver "Chaveiro" no item da caça), auto-compra, facas e luvas. Auto-compra e confirmação sem celular: plano futuro em
`docs/superpowers/plans/2026-11-06-auto-compra.md`.

## Arquitetura

- Um binário `skin-sniper.exe` (crate único, Rust estável, tokio). Crates: axum (HTTP + SSE), reqwest
  (rustls, json, form, multipart, query), serde/serde_json, rusqlite (`bundled`), rust-embed, tracing.
  Nada além disso sem motivo escrito no PR.
- Frontend: Svelte 5 + TypeScript + Vite em `web/`; `npm run build` gera `web/dist`, embutido no binário
  com rust-embed. npm só no build; o usuário roda só o `.exe`.
- Dados em `data/` (fora do git): `sniper.db` (SQLite, WAL) e `sniper.log` (rotação 5 MB × 3).
- Porta padrão `0.0.0.0:8788` (o robô antigo usa 8787 e fica ligado em paralelo até a troca).
- Metas de peso, medidas no fim: CPU ociosa < 1 %, RAM < 80 MB, `data/` < 300 MB em regime.

### Módulos (`src/`)

| módulo | faz |
|---|---|
| `db.rs` | abre SQLite, migrações, retenção (job de hora em hora) |
| `settings.rs` | chaves e preferências no banco; leitura mascarada (só os 4 últimos caracteres) |
| `steam/client.rs` | HTTP único da Steam: só GET público (sem cookie de conta), limitador adaptativo, breakers por endpoint |
| `steam/listings.rs` | código G, `QueryListingsForItem` (com `propertyFilters`), parse de `asset_properties` |
| `steam/history.rs` | `QueryPriceHistory` → pontos `(ts, mediana_centavos, compras)` |
| `steam/book.rs` | `/market/orderbook` `Load` → maior pedido de compra, quantos pedidos abertos e os 5 primeiros degraus de venda |
| `steam/search.rs` | `search/render` para descoberta de catálogo (USD → BRL por razão medida) |
| `csfloat.rs` | `/history/<nome>/sales` → múltiplos `price / reference.base_price` por venda |
| `pricing/fees.rs` | taxa em centavos inteiros e inverso por busca binária |
| `pricing/sell.rs` | três preços de venda, prazo, confiança |
| `pricing/premium.rs` | prêmio do raro que fecha (seed → grupo → 1×) e o selo "comprovado" |
| `pricing/groups.rs` | grupo raro de um anúncio (tier de pattern, fase Doppler, faixa de fade) a partir das tabelas |
| `pricing/combo.rs` | melhor combinação de anúncios que cabe no saldo (mochila em centavos) |
| `hunt/catalog.rs` | descoberta e seleção do que vigiar dado o saldo |
| `hunt/bootstrap.rs` | catálogo curado embutido, referências públicas separadas e renovação diária sem Steam |
| `hunt/scheduler.rs` | fila de consultas por prioridade dentro do ritmo do limitador; escolhe a faixa rara pelo índice marginal (`best_range`) |
| `hunt/detect.rs` | anúncio novo → avaliação → oportunidade / some quando vendido |
| `hunt/ledger.rs` | registro de observações: todo anúncio visto, idade, sumiço e relistagem (aprendizado) |
| `hunt/teacher.rs` | professor grátis: compras virtuais dos avisos e quase-achados e o resultado delas no mercado; recalibração do comum |
| `hunt/value.rs` | mede o estoque acumulado (exposure) no aprendizado e ajusta os parâmetros a cada hora (Aprendizado, item 6) |
| `research.rs` | atribuição de decisões e tentativas, cota persistente de pesquisa |
| `hunt/explore.rs`, `hunt/trial.rs` | candidatas fora do ranking, continuidade de ativos e comparação por blocos |
| `notify.rs` | Telegram + evento SSE para notificação/som no navegador |
| `web/` | rotas, SSE, guarda de Host, arquivos embutidos |

Tabelas de pattern copiadas do projeto antigo para `assets/patterns/` (ver `conhecimento.md` 4.5). `floats.json` traz o
menor e o maior float de cada skin (ByMykel/CSGO-API, via `skins.json` do projeto antigo; facas e luvas fora).

## Contrato da API (o frontend depende só disto)

Valores de dinheiro sempre em **centavos BRL inteiros**. Datas em segundos Unix.

```
GET  /api/state            -> State
PUT  /api/saldo            {cents: int}            -> State      (saldo manual; null apaga)
GET  /api/opportunities    -> Opportunity[]         (ativas e todas as perdidas nas últimas 24 h, com gone_at; o frontend ordena)
GET  /api/opportunities/:id/history -> {points: [[ts, median_cents, purchases]], sell: Sell}
POST /api/buy              -> 410 (compra só na Steam; nada sai pela rede)
GET|POST /api/steam/login/* -> 410 (sem login)
POST /api/opportunities/:id/bought -> Purchase     ("Comprei" na Steam; ativa ou perdida; a ativa sai com reason "bought")
GET  /api/purchases        -> Purchase[]            (comprada e não vendida traz `now`: quanto vale hoje, sem consulta nova)
DELETE /api/purchases/:id  -> null                  ("Não comprei"; 404 se já tem venda informada)
POST /api/purchases/:id/sold {sold_cents: int}      -> Purchase (valor pago pelo comprador, com taxas)
GET  /api/calibration      -> Calibration
GET  /api/watch            -> WatchRow[]            (o que a caça vigia; mais perto de compensar primeiro)
GET  /api/learning         -> Learning              (placar do aprendizado, 60 dias de Brasília)
GET  /api/settings         -> Settings              (chaves mascaradas)
PUT  /api/settings         Partial<SettingsInput>   -> Settings   (campo null apaga; steam_login_secure não nulo → 400)
GET  /api/events           SSE: "opportunity" Opportunity | "gone" {id, at, reason} | "state" State | "purchase" Purchase
```

```ts
State = {
  saldo_cents: number, saldo_source: "steam" | "manual" | "none",
  hunt: { watching: number, queries_per_min: number, last_query_at: number | null,
          paused_until: number | null, last_ok_at: number | null,   // última resposta sem 429
          book_ok_at: number | null,   // última resposta boa da fila pública, se ainda disponível; null em pausa
          message: string | null },
  combo: { ids: string[], cost_cents: number, profit_cents: number },   // melhor combinação no saldo
  keys: { steam: KeyStatus, csfloat: KeyStatus, telegram: KeyStatus }
}
KeyStatus = { set: boolean, ok: boolean | null, detail: string | null }  // detail: conta, erro...
Opportunity = {
  id: string,                    // listingid
  name: string,                  // market_hash_name
  icon_url: string, steam_url: string, inspect_url: string | null,
  wear: string | null, float: number | null, seed: number | null,
  group: string | null,          // ex.: "T1 Center Star", "Ruby", "Fade 99%", "Float ≤ 0.173"
  kind: "desconto" | "raro",
  price_cents: number,           // total que o comprador paga
  sell: Sell,
  margin_pct: number,            // lucro no preço médio / preço
  safe: boolean,                 // lucro > 0 mesmo no preço rápido
  confidence: "alta" | "media" | "baixa",
  ev_profit_cents: number | null,  // lucro estimado, em centavos
  days_locked: number | null,      // trava e prazo estimado de venda, em dias
  profit_per_day_cents: number | null, // lucro estimado dividido pelo prazo total
  risk: "baixo" | "medio" | "alto" | null,
  evidence: string,              // frase curta: "392 vendas/dia nos últimos 7 dias"
  seen_at: number,
  checked_at: number | null,     // última consulta que devolveu este anúncio; não é a criação na Steam
  gone_at: number | null,        // fica visível por 24 h após sair, inclusive depois de reiniciar
  gone_reason: "unavailable" | "unprofitable" | "unwatched" | "bought" | "rejected" | null,
  seal: Seal | null              // só raro: selo do prêmio (Aprendizado, item 5)
}
Seal = {
  proven: boolean,               // "comprovado": ≥ 10 vendas fechadas e o pior quartil ainda passa na régua
  sales: number, parked: number, // vendas fechadas usadas; anúncios do grupo parados há mais de 48 h
  p25_profit_cents: number | null,
  open_cents: number | null, floor_cents: number | null,  // anunciar a base × p75 e descer até base × prêmio
  text: string                   // "ainda não comprovado: 4 de 10 vendas fechadas"
}
WatchRow = {
  name: string, icon_url: string,
  floor_cents: number | null,      // menor anúncio visto na última consulta; null se ainda não consultou / página vazia
  max_buy_cents: number | null,    // maior preço que ainda dá lucro >= R$ 1,00 e >= 10 % no preço médio;
                                   // null sem estimativa e sempre null em item raro (o teto depende do grupo)
  sales_per_day: number, kind: "desconto" | "raro", group: string | null,
  last_checked_at: number | null,  // ordem: (floor - max_buy) crescente; sem os dois, por último
  mode: "barato" | "seed" | "float", // modo de busca do alvo (a mesma skin pode aparecer em mais de um modo)
  score: number,                   // lucro esperado por consulta gasta, em centavos, aprendido (0 sem dados)
  reason: string                   // por que a agenda dá consultas a ele, em palavras curtas: "rendeu R$ 1,20 em
                                   // 30 consultas", "testando alvo novo", "quase deu lucro 3 vezes",
                                   // "sem achar nada há 5 h", "sem achar nada em 40 consultas"
}
Sell = { high: Tier, mid: Tier, quick: Tier }
Tier = { price_cents: number, net_cents: number, profit_cents: number, eta_hours: number | null }
Purchase = { id, name, icon_url, price_cents, status: "pending" | "need_confirmation" | "bought" | "failed",
             message: string | null, bought_at: number | null, tradable_at: number | null, sell: Sell,
             kind: "desconto" | "raro" | null, sold_cents: number | null,
             wear, float, seed, group,                   // copiados da oportunidade
             now?: Sell }                                // valor de hoje (`hunt::detect::value_now`), nunca guardado
Calibration = { families: [{ family: string, kind: "desconto" | "raro", n: number,
                              mean_error_pct: number, bias_pct: number }],
                hours: [{ hour: 0..23, listings: number, opportunities: number }] }
Learning = { days: LearningDay[], since: number | null,     // do dia mais recente; since = 1º anúncio do registro
             teacher: TeacherSummary,                     // resumo das compras virtuais (Aprendizado, item 4)
             value: ValueInfo | null,                     // parâmetros da agenda e medidas (item 6); null até o 1º ajuste
             refit: number,                               // fator do professor no comum, 1 = sem desconto (item 7)
             weeks: [LearningWeek, LearningWeek],         // boletim: últimos 7 dias e os 7 anteriores
             research?: ResearchSummary | null,          // tentativas e cota na última hora
             trial?: TrialInfo | null,                   // referência, controle, piloto, pausa ou fim
             exploration?: ExplorationInfo | null,       // candidata, visitas e cobertura nova
             cohorts?: LearningCohort[],                 // decisão original; comum e raro separados
             experiment?: TrialReport | null,            // comparação por blocos e incerteza
             adjustments: { at, old, new, why }[] }      // ajustes registrados no fator dos comuns
LearningDay = { day: "AAAA-MM-DD", queries, books, new_listings, aged, age_median_min: number | null,
                sales, relists, life_median_min: number | null, near, finds, price_samples,
                price_error_pct: number | null, price_bias_pct: number | null } // medianas dos erros dos avisos comuns com preço em 48 h, agrupados pela data da previsão
Settings = { steam_login_secure: string|null, csfloat_key: string|null, telegram_token: string|null,
             telegram_chat_id: string|null, panel_url: string|null, sound: boolean,
             notify_min_profit_cents: number }      // segredos chegam mascarados: "••••gLO9"; steam_login_secure é sempre null
```

## Acesso ao painel

- Sem login nem token (decisão do Erick). Escuta em todas as interfaces.
- Duas proteções invisíveis contra outro site disparar compra pelo navegador do Erick:
  1. Host aceito só se for IP literal, `localhost`, `*.ts.net` ou o nome do PC (anti DNS rebinding).
  2. Requisição que muda algo exige `Content-Type: application/json`; o servidor não manda cabeçalhos CORS
     (força preflight que falha para outra origem).

## Caça

Registro para comparação: `reads` recebe `choice_range` (alvo + início + fim),
`choice_gap` (intervalo real na escolha), `choice_control`, `eligible_ranges`
(quantidade de faixas elegíveis), `oldest_range` e `oldest_gap` (mais velha elegível;
intervalo nulo se nunca vista). O retrato é consumido uma vez no resultado principal.
Paralelas, conferências, livros e leituras sem escolha comparável deixam os novos campos
nulos. Migração aditiva conserva linhas antigas com nulos. Não muda escolhas nem corta leituras.
`outcome` marca a leitura com escolha: `ok`, ou `failed` quando a consulta saiu para a Steam e falhou
(retrato gravado uma vez, `new_listings` nulo, sem nota; o ajuste ignora essas linhas). Nulo nas demais.
A falha devolve à faixa a leitura anterior (ou nenhuma), sem mudar o descanso existente: por `data` nula ela volta a concorrer depois do descanso; nos demais erros pode voltar já na próxima escolha, e os registros de falha por faixa mostram a repetição.

Comparação offline independente: `SNIPER_COMPARISON_DB=<cópia> SNIPER_COMPARISON_SINCE=<unix> SNIPER_COMPARISON_UNTIL=<unix> cargo test compare_recorded_reads -- --ignored --nocapture`.
Abre apenas a cópia em modo somente leitura, sem migração, configurações, caça ou rede.
Só conta leituras `ok` com `at` no período comum (início e fim inclusivos), o mesmo para os dois
braços, e o relatório informa o período. Falhas não viram zero; vazio bem-sucedido conta zero.
Cada faixa exata e banda (`<600`, `600-3600`, `3600-21600`, `>=21600` segundos)
precisa de 30 leituras em CADA braço; abaixo disso informa dados insuficientes.
`fresh` mede anúncios raros novos dentro do saldo por leitura, creditados uma vez.
`new_listings` guarda todos os novos devolvidos pelo registro, sem recorte de saldo.
Falha de registro mantém nulo, nunca zero; leituras antigas também ficam nulas.
O relatório mostra as duas medidas separadamente por braço/faixa/banda, cada uma com
mínimo de 30 leituras válidas em cada braço; nulos não entram na medida de todos os novos.
Testes offline cobrem totais distintos (4 novos contra 1 no saldo), nulos e o limiar 29/30.
Primeiras leituras são excluídas. Bandas ainda contêm esperas diferentes, e seleção,
saldo e mercado podem variar. O relatório mostra associação com a agenda, sem causalidade.


1. **Catálogo e descoberta:** ao subir, importa `assets/catalog/curated.json` sem apagar medições anteriores.
   Só os nomes da curadoria participam das novas seleções; itens removidos continuam no banco para histórico
   e acompanhamento de oportunidades ativas. Nome-base, desgaste e variante são distintos; códigos gerados
   só usam metadados sem ambiguidade. Referências USD ficam em `catalog_references`, nunca em campos de piso,
   giro, dispersão ou histórico medidos. Um GET público diário renova apenas essas referências; falha conserva
   o cache e tenta novamente em uma hora. Referência com mais de 7 dias não faz triagem. Vendas Steam realmente
   capturadas na última semana também podem fazer triagem em BRL, sem converter média em anúncio mínimo.
   A fila de históricos intercala duas candidatas novas para uma renovação vencida dentro das mesmas vagas;
   as faixas de float podem ser pesquisadas abaixo do piso mínimo do desconto. Giro e estimativa continuam
   exigindo histórico real. **Descoberta Steam (1×/dia e quando o saldo muda muito):** `search/render` ordenado por popularidade nas
   categorias de arma, sem faca/luva, até cobrir os itens com piso ≤ saldo. Para cada candidato,
   `QueryPriceHistory` (cache 6 h) dá vendas/dia e dispersão.
   A mesma parcela de consultas renova primeiro os históricos dos comuns vigiados nos 5 min antes de vencer.
   Durante uma pausa das actions, usa o histórico antigo autenticado somente se a rota principal estiver
   disponível; a saída pública não libera essa leitura. Erro enviado também gasta crédito; sem envio,
   a agenda segue para a caça normal. A candidata externa continua fora dessa prioridade.
   Nessa pausa, as mesmas vagas também enriquecem candidatos do catálogo depois de renovar os vigiados;
   sem fila autenticada disponível, aguardam. Não há consulta adicional fora da cota de descoberta.
2. **Seleção:** (os alvos comuns em desconto deste item estão **desligados desde 11/11** — ver "Caça de comum em
   desconto desligada" abaixo; valem os de seed e de float) vigia itens com piso entre o mínimo viável e o saldo e ≥ 1 venda/dia. Nota = vendas/dia
   (até 10: quem compra uma unidade não ganha com giro maior) × lucro possível em reais × fator aprendido do alvo (abaixo). Lucro possível = `seller_gets(piso ÷ (1 − dispersão))` − piso,
   com dispersão (P50−P10)/P50 limitada a 50 %: o preço da taxa já entra, então item barato ou de pouca folga fica
   no fim da fila (não sai: um anúncio fora da curva ainda pode dar lucro). Mais **uma** consulta de grupo raro
   por skin com tabela (código G, **sem filtro de desgaste**: a mesma consulta traz todos os desgastes, StatTrak e
   Souvenir da faixa) com seeds admitidas pela prova de vendas, inclusive quando o preço visto do raro passa do saldo (`propertyFilters` por faixa de seed dos
   tiers; a tarefa percorre as faixas em rodízio, uma por consulta). Até 24 raros, os que mais vendem por dia
   primeiro (soma dos desgastes; sem venda medida conta 1/dia): giro alto traz anúncio novo, e a revenda sai mais
   rápido. Antes era uma tarefa por desgaste: o Moonrise ocupava 5 vagas e cada faixa era revista 5× menos.
   - **Piso:** o do anúncio visto na última consulta da tarefa (vale 6 h); sem consulta ainda, USD × razão medida,
     que só serve de triagem (a Steam em BRL não segue a razão por item; o raro T1 chega a custar dezenas de vezes
     o piso comum da skin). Desconto com anúncio visto acima do saldo sai; raro continua elegível mesmo quando todas as faixas
     vistas estão acima do saldo. Um anúncio novo barato pode aparecer depois. O raro com piso visto acima
     do saldo recebe 1/4 da nota-base, sem perder a elegibilidade. A exceção é o raro fora de alcance, a seguir:
     ali não é o preço visto do raro que o tira, é nem o comum mais barato da skin caber no saldo.
     **Fora de alcance (18/11):** sai da caça a skin rara cuja estimativa do anúncio mais barato (todos os desgastes,
     StatTrak e Souvenir que o catálogo conhece, dentro ou fora da curadoria, pela triagem USD × razão) passa do
     saldo com folga de 10 %: se nem o mais barato cabe, nenhum cabe. A folga vem da medida de 18/11/2026 no banco instalado: o piso em BRL ficou entre 0,92 e 1,02 × a
     triagem em 98 % dos 1.085 itens com piso de R$ 1,00 ou mais (mediana 0,95, menor 0,915). O que a folga não cobre
     é preço velho (mediana de 11,7 h e máximo de 322 h nas 206 variantes dos alvos lidos naquele dia); por isso a
     skin fica quando alguma variante está sem preço de triagem, quando tem oportunidade ativa (a qualquer preço; ela também passa à frente no limite de 24 alvos), ou quando a leitura
     mais recente (com até 6 h) de alguma faixa de seed ou de algum nome na varredura do piso trouxe anúncio dentro
     do saldo. Vale a leitura mais recente de cada faixa ou nome: se ela veio vazia ou só com anúncio acima do
     saldo, o anúncio barato de antes já não está lá e a estimativa volta a decidir; o mesmo depois de reiniciar,
     quando os pisos vistos recomeçam vazios. Variante que o catálogo ainda não conhece não entra na conta. Não medido: quantas vezes a estimativa velha tira uma skin que caberia.
     Medido nas 24 h até 02:15 de 18/11, com saldo de R$ 41,91: 298 das 1.671 leituras de faixa (18 %) foram para 3
     skins assim (Five-SeveN | Case Hardened, a mais lida de todas, com 276 leituras e comum mais barato a R$ 63,04;
     MAC-10 | Case Hardened a R$ 121,49; Galil AR | Phoenix Blacklight a R$ 417,71). Com isso deixa de existir o aviso
     de anúncio bom acima do saldo para essas skins (nenhum `acima_do_saldo_boa` nos 14 dias até 18/11); ele continua
     para as skins cujo comum cabe no saldo. Quando o saldo sobe, elas voltam a concorrer na seleção seguinte (o
     limite de 24 alvos continua valendo).
     Medido no programa instalado nos 67 min depois da troca (02:38:50 a 03:46:09 de 18/11), contra os 66 min anteriores:
     leituras nessas 3 skins de 38 para 0; leituras de faixa com resposta de 89 para 99 por hora e com erro de 36
     para 21 por hora; páginas da varredura do piso de 115 para 121 por hora; total de 315 para 313 tentativas por
     hora; `principal_429`, `extra_429` e `livro_429` em zero nas 6 janelas de 10 min do registro depois da troca; nenhuma oportunidade nova nos dois períodos.
     A seleção é refeita a cada 15 min e no próximo passo após observar um piso novo acima do saldo.
     Entrada rara exige código, desgaste normal, referência disponível e seed admitida por `patterns::learn`
     para aquele desgaste; sem medida alguma usa as tabelas de instalação. Os limites da prova não mudam.
     Reconsidera a prova ao renovar vendas e a seleção a cada 15 min ou mudança relevante do saldo,
     mantendo o limite de 24 skins. Ausência de vendas não autoriza inventar prêmio nem admitir candidato.
     A medição offline do funil está em `medida-catalogo-2026-11-15.md`.
   - **Agenda que aprende:** cada alvo (skin × desgaste × modo: `barato`, `seed` ou `float`) recebe a leitura
     conforme a prioridade calculada: no modo `seed`, a faixa de alvo raro usa o índice marginal; nos demais modos, usam-se os centavos esperados, calculados pelo rendimento e pelo estoque acumulado durante a espera (ajuste e medidas no item 6 do Aprendizado). Sinal =
     anúncio **novo** que cabe no saldo (na leitura só pelo livro, a queda do menor anúncio); reavaliar o mesmo
     anúncio não conta de novo. Alvo nunca lido vai na
     frente; empate vai para o lido há mais tempo. Regras que não mudam: 12,5 % das consultas vão sempre ao alvo
     menos testado, com o slot girando entre os modos (cada modo tem piso próprio, qualquer que seja o número de
     alvos). O saldo e a taxa continuam decidindo quais anúncios viram oportunidade (regras acima); a nota-base do catálogo só desempata
     alvos ainda não testados. A busca e as consultas de anúncios dividem o mesmo limitador global. As notas ficam
     no banco (`arm_stats`, retenção 14 dias e no máximo 2000 linhas) e sobrevivem a reinício.
   - **Fator aprendido na seleção (desconto):** rendimento do alvo `d:` (taxa × retorno, já encolhido para a média
     da classe quando há pouca medida) ÷ a mediana dos alvos `d:` já testados, entre 0,25 e 4. Sem referência
     (ninguém mediu ainda), o fator é 1. Quem rende pouco perde a vaga (`WATCH_MAX`) para uma skin do catálogo
     ainda não testada, que entra com fator 1 e é testada logo. Raro e float seguem por vendas/dia. Os limites
     (0,25–4) são hipótese de calibração, não medida.
   - **Piso mínimo de desconto:** menor preço em que uma venda 30 % acima ainda deixa R$ 1,00 e 10 % depois da taxa
     (hoje ≈ R$ 7,57). Abaixo disso a taxa come o lucro: até 06/11, 9 000 consultas em skins de R$ 1,60–8,50 deram
     um achado de R$ 0,32. Os 30 % são hipótese, não medida.
3. **Ritmo:** teste limitado aprovado pelo Erick em 15/11/2026 em lugar do teto anterior de ~100 consultas/h: descanso de anúncios começa em 28 s,
   reduz 2 s após cada 6 h de operação sem 429 em Listings, até o piso autorizado de 20 s.
   Qualquer 429 em Listings, na principal ou extra, volta a 28 s e impede redução por 24 h desde
   o último 429. Depois desse prazo, espera mais 6 h limpas (interpretação conservadora).
   Degrau e prazo sobrevivem ao reinício; a janela de 6 h recomeça ao abrir, sem contar tempo fechado.
   A 20 s, o teto aritmético é 180 leituras/h antes das demais esperas; não foi medido como seguro.
   Grupos de controle independentes: actions de anúncios/histórico; livro/busca/interface antiga;
   sessão/login/compra. São divisões operacionais baseadas nas respostas medidas, não cotas oficiais da Valve.
   Sessão e compra não herdam o 429 de uma varredura de anúncios. Entre duas consultas quaisquer há no mínimo 3 s, de
   qualquer balde ou saída: nunca rajada. As leituras de mercado somadas (todos os endpoints) têm ainda intervalo médio de 12 s (300/h), com variação de ±30%, inclusive
   entre endpoints diferentes e na primeira leitura após reiniciar; sessão, login e compra mantêm a fila própria.
   Cada balde tem o próprio intervalo e a própria pausa por 429, preservados no banco. Com um balde em pausa e a saída extra sem vaga, a consulta falha na hora
   em vez de segurar a caça. Com as actions barradas, alvos comuns usam `/render` da interface antiga e o histórico
   não tem rota pública (o `/pricehistory` antigo exige login; contratos em `pesquisa-web.md` 10). Histórico com erro HTTP ou JSON
   inválido espera 10 min antes de repetir o mesmo nome, compartilhado entre descoberta e avaliação. É hipótese de recuo; falha nunca renova o histórico salvo. Filtros de seed/float
   continuam nas actions: uma página comum não equivale à consulta filtrada. A seleção aprendida continua sendo
   atualizada a cada 15 min, mesmo durante essa pausa; descoberta de nomes novos e raros filtrados aguardam as actions.
   Comuns também usam o render da principal quando sua próxima vaga vem antes da fila nativa ou quando as
   actions estão em pausa/breaker, mesmo sem a saída extra; a pausa das actions não deve forçar triagens por
   livro/busca na fila pública disponível. Pausa ou breaker do próprio render continuam impedindo a preferência. O espaçamento
   global, o intervalo do livro/render e as pausas de cada fila continuam valendo. Nessa situação, a agenda
   lê a página diretamente, antes das triagens por livro ou busca que gastariam a mesma fila disponível.
   Com o livro barrado, não há pedidos sugeridos. Com as duas leituras barradas, a caça para.
   O balde de anúncios e histórico: começa com 1 consulta a cada 6 s (10/min; com 3 s e 4 s houve 429 por horas, e o robô antigo leu a ~6–12/min sem 429), que também é o piso. Toda leitura é anônima. O prazo `Retry-After`, quando informado, é respeitado. Um 429
   pausa 60 s (dobrando até 30 min: com tentativas a cada 10 min a penalidade de 04/11 não esfriava) e alonga o intervalo 50 %; cada 10 min sem 429 encurta 15 % (de 60 s a 6 s em ~2,5 h), até o piso aprendido: o 1º 429 de uma série sobe o piso para 1,25× o intervalo em uso (até 20 s), e cada 6 h sem 429 desce um degrau até 6 s (a 6 s fixos a Steam bloqueava a cada 1–2 h, e cada série custava até 2 h paradas). O degrau da pausa só volta a 60 s após 60 min sem 429.
   Pausa, intervalo, piso aprendido e a última resposta sem 429 ficam salvos em todas as filas (`steam_pace`)
   e valem depois de reiniciar; o tempo quieto desde o último encurtamento salvo continua encurtando. O valor
   conserva o texto nativo antigo antes do primeiro `;`, seguido dos estados público, extra e de controle.
   Na primeira atualização de um valor antigo, recusas públicas registradas há menos de 30 min recuperam um
   descanso de 30 min desde a última recusa, sem zerar a pausa nativa existente. Novas recusas públicas após
   o descanso aumentam a pausa para 60 min, 2 h, 4 h e depois 6 h; uma recusa de outra consulta já em voo não avança
   esse degrau novamente. A série pública só recomeça após 12 h sem 429, preservada ao reiniciar, e
   `Retry-After` maior continua valendo. O estado é salvo em todo 429
   e conferido a cada 30 s. Breaker por endpoint.
   Recusa pública espera pelo menos 30 min (ou `Retry-After` maior). Se as duas filas de leitura da
   principal já foram recusadas, ambas aguardam o maior prazo, evitando tentativas desencontradas na
   mesma saída; uma fila saudável não herda a recusa da outra.
   Os números ficam no `State` para o painel mostrar. Página antiga validada renova a última leitura boa e
   mantém o painel ativo enquanto sua fila estiver disponível; não apaga a pausa das actions. Tentativa que
   termina sem enviar requisição não aumenta as consultas do aprendizado nem o crédito de descoberta.
   O painel identifica consultas/min como totais (incluindo livro, busca e histórico). O cabeçalho resume
   o ciclo em tempo, anúncios únicos e novos no monitoramento. O log preserva
   páginas processadas, avaliações, aprovações e recusas. Repetições não aumentam os distintos;
   novo no monitoramento não prova criação recente na Steam.
   Filtros de seed/float em pausa continuam sinalizados mesmo quando o render mantém os comuns ativos.
   **Saída extra só com IP diferente (09/11):** a cada 5 min o app confere o IP público das duas saídas
   (`api.ipify.org`, fora da Steam) e só usa a extra quando é diferente do de casa; conferência falha ou IP igual
   = extra parada. Em 08/11 as duas saíram pelo mesmo IP (PR #88) e somaram ~1 000/h, abrindo um bloqueio de mais
   de 1 dia; em 09/11, com o IP de casa ainda em 429, a Starlink devolveu 200 em `/actions`. Com IP diferente, a
   extra leva toda leitura pública (anúncios, histórico, livro, busca, interface antiga) e tem intervalo médio próprio de
   12 s entre leituras, com variação de ±30%; o mínimo de 3 s entre quaisquer envios continua somando as duas. Desenho anterior: um proxy HTTP em `127.0.0.1:1056` (`ALT_PROXY`; no PC do Erick é o `saida-celular`, que sai pela
   rede fixa autorizada por um celular com Tailscale; redes atuais em `pesquisa-web.md` 13) dá outro IP público,
   sem garantia de cota independente. As sondas de
   `pesquisa-web.md` 11 encontraram actions recusadas e livro/render públicos disponíveis nessa rota. Por isso a
   produção usa a extra só para livro e anúncios antigos públicos; histórico, actions filtradas, compra, sessão e
   login ficam na principal. A extra nunca recebe o cookie de login. Tem limitador próprio,
   persistente; compartilha com a principal os pisos de 3 s entre quaisquer envios e 10 s entre leituras,
   além do intervalo adaptativo por grupo.
   Anúncios comuns aproveitam a alternativa pública livre enquanto a principal aguarda o ritmo; a cobertura
   continua por nome e o leitor valida BRL, identidade e paginação. Recusa, erro de rede ou 429 na extra não tocam no breaker nem no limitador da principal: a
   consulta volta para a fila dela, e a extra descansa 5 min (rede/recusa) ou pausa pelo próprio 429. Consultas por
   minuto somam as duas; a caça só aparece em pausa quando as duas estão paradas. Sem o proxy ligado, nada muda.
   Além da fila principal, uma única leitura pública em paralelo pode consultar alvo comum já selecionado, sem
   usar login nem retornar pela principal. Ela reserva a próxima vaga em até 6 s, antes das próximas triagens
   do livro; uma pausa por 429 ou falha de rede impede a reserva e é reconferida antes do envio. Respeita o limitador da extra e começa no máximo uma
   página a cada 12 s, com pelo menos 60 s entre visitas ao mesmo alvo. Não disputa ativos atrasados ou livro
   periódico pendente; só usa histórico recente e não baixa histórico nem CSFloat na análise paralela. O laço
   principal valida e processa a página: se outra consulta da mesma skin começar antes do retorno, a seleção
   mudar, o alvo ficar ativo ou o resultado passar de 20 s, descarta. Uma leitura validada entra no registro e
   na nota do alvo, identificada como `paralela`, sem inflar a cota externa nem se misturar aos blocos do ensaio.
   A referência de alvos comuns conta achados e consultas normais e paralelos; cada anúncio rende crédito uma vez.
   Uma página comum lida há menos de 60 s é compartilhada entre a agenda principal e a paralela: escolhem outro
   alvo em vez de gastar outra consulta nela. Oportunidades ativas continuam sendo reconferidas; os filtros
   de seed/float e a prioridade de acompanhar ativos atrasados não mudam.
4. **Triagem e detecção:** tarefas de desconto da mesma skin-base compartilham uma busca; preço em USD convertido pela razão medida até 10 % acima do teto prioriza anúncios. A tolerância e o frescor do preço da busca são hipóteses, sem medição. Resultado ou teto ausente mantém consulta direta; oportunidades ativas também são conferidas.
   Busca em pausa por 429 ou suspensa por falhas é dispensada: o alvo segue pelo livro e pelos anúncios disponíveis,
   mesmo sem a saída do celular. O primeiro 429 da busca também faz essa troca no mesmo passo, respeitando as pausas de cada consulta.
   Alvo comum sem outra versão para a busca cobrir e com teto lê antes o livro de ofertas (menor anúncio em R$, no
   outro balde): acima do teto, com a mesma tolerância de 10 %, os anúncios não são lidos, e só a **queda** do menor anúncio
   (dentro do saldo) ensina a agenda como um anúncio novo: a subida é venda ou retirada, e o primeiro livro depois
   de ligar não tem com o que comparar. **Descanso da leitura de anúncios (10/11):** alvo raro ou de float só é
   escolhido após o descanso adaptativo do item 3 desde a última leitura de anúncios. **Descanso do alvo que
   falha (12/11):** alvo cuja consulta volta `data` nula descansa 5 min, dobrando a cada falha seguida até 1 h, e
   volta ao normal na primeira leitura que der certo; antes era escolhido de novo ~80 s depois e dois alvos
   gastavam 36 % das tentativas de anúncios falhando (`conhecimento.md` 1.3.1). **Foco em seed e float (11/11):**
   passado o descanso, a vaga é sempre de um alvo de seed ou de float, e entre eles decide a agenda (valor da
   leitura). De 11/11 a 12/11 foram duas de seed para cada uma de float, fixo: 950 leituras de seed deram 29
   anúncios novos em 24 h, contra 200 em 850 de float. Solta, a agenda deu 66 de seed para 6 de float numa hora;
   em 12/11 o float ganhou piso de uma leitura a cada duas raras (#114), desfeito no mesmo dia com "Só pattern"
   (abaixo). **Caça no CSFloat (12/11): removida no mesmo dia.** O feed dos anúncios mais novos do CSFloat foi lido por
   algumas horas e não deu achado que se confirmasse: a API entrega o anúncio ~60 s depois de criado, o próprio
   site já mostra o preço previsto a quem anuncia e comprar lá exige dinheiro depositado (`pesquisa-web.md` 5.1).
   Erick decidiu não gastar consulta nisso. As tabelas `csfloat_deals` e `csfloat_feed` ficam no banco, sem uso;
   as vendas do CSFloat (`csfloat_sales`) seguem como referência de preço do raro na Steam.
   Antes a vaga livre disputava com os alvos baratos pelo valor da leitura e, em 12 h, foram 1 869 leituras de
   barato, 531 de seed e 87 de float, sem achado. O crédito de descoberta caiu de 0,25 para 0,125 por consulta
   (≈ 11 % das vagas): eram 42 históricos de catálogo por hora no mesmo balde dos anúncios.
   **Só pattern (12/11, pedido do Erick):** float deixou de criar grupo raro e alvo de busca (saem os alvos `f:` e
   `h:` e os grupos `float:`/`highfloat:`); a leitura de anúncios vai inteira para alvos de seed. Um seed só entra
   na busca quando as vendas guardadas do CSFloat provam que ele paga (regra em "Precificação", "Catálogo de
   pattern"); skin já medida é consultada só nos seeds pagantes, e skin de tabela ainda sem medida não é lida
   (a tabela inteira só vale em instalação nova, sem nenhuma skin medida). A agenda passa a escolher a **faixa
   de seed**, não o alvo (item 6 do Aprendizado), e 1 leitura em 8 vai para um rodízio cego que serve de régua.
   O texto abaixo sobre float e sobre a vez fixa de seed e float fica como histórico.
   **Varredura do piso (17/11):** a consulta por faixa do Desert Eagle | Heat Treated voltou com
   `data` nula em 16 de 18 leituras no log a partir das 16h e em 18 de 19 na bancada, com seis formatos
   de consulta. Em paralelo à agenda de faixas, a interface antiga lê os 100 mais baratos de um nome
   e avalia todos os seeds da página na tarefa rara. A prioridade é tempo sem ler × seeds pagantes ×
   vendas por dia, com preferência para nomes inéditos e filtro de saldo. O intervalo entre
   varreduras (60 s em 17/11; desde 18/11, a regra de vez sim, vez não descrita abaixo) e o mínimo de 180 s por nome são hipóteses a ajustar pela medição das consultas e dos achados.
   **Metade das leituras para o piso (18/11):** vez sim, vez não: depois de cada turno da caça que enviou leitura (faixa, item ou acompanhamento de oportunidade) a varredura usa a vaga de leitura seguinte; sem esse turno ela espera 24 s (duas vagas), o que a limita a cerca de 150 leituras por hora. O teto de leituras por hora não muda, porque a varredura espera a mesma vaga das outras consultas (12 s em média, cerca de 300 por hora); o total pode subir até esse teto, porque a varredura passa a usar vagas que ficavam livres (medido em 18/11/2026 antes da mudança, em 15 min: 56 tentativas na Steam, cerca de 224 por hora, 13 delas da varredura). Um turno da caça pode ter mais de uma leitura (medido nas 48 h até 18/11/2026, origens `normal` e `exploracao_interna`: 4.137 turnos com 4.956 leituras de anúncios, 1,2 por turno, 819 deles com duas), então a varredura não chega à metade das vagas; a parcela real fica a medir no programa instalado. Limite conhecido: a regra conta o turno, não a vaga usada; com a saída extra ligada (nenhuma das 5.643 leituras da Steam dessas 48 h passou por ela) a varredura poderia usar todas as vagas da saída principal, e a regra precisa ser revista antes de religá-la. Medido em 18/11/2026 no banco instalado: 101 nomes de skins com tarefa de seed (nem todos com seed pagante no próprio desgaste) tinham piso até o saldo de R$ 41,91 e somavam 6.585 vendas por dia; os 12 com mais de 190 vendas por dia somavam 4.045 (61 %). Nas 48 h anteriores, as 4.129 leituras de faixa registradas pela agenda trouxeram 83 anúncios novos, em 59 leituras (1,4 %), com intervalo médio de 9.924 s entre leituras da mesma faixa; nenhum virou oportunidade. As 10 oportunidades registradas de 11 a 14/11 ficaram no ar, entre o aviso e o sumiço visto pelo programa, 3, 3, 10, 15, 29, 37, 75, 96, 158 e 199 min (mediana 33 min; só entram as que duraram até a primeira leitura). Com a correção da espera pela vaga instalada, as varreduras saíram a cada 60 a 62 s (04:14:15, 04:15:15 e 04:16:17 UTC). A hipótese de 17/11 (anúncio raro no piso de um nome que vende N por dia dura cerca de 1.440 ÷ N min) não se confirmou. Medido em 18/11/2026 no banco instalado, tabela `observations`, 7 dias: dos anúncios comuns com preço até 1,05 × o piso, 563 vistos e 275 sumidos, vida mediana de 31 min (quartis 12, 31 e 68); nos nomes com 100 ou mais vendas por dia a mediana foi 34 min e nos demais 238 min (só 16 casos). Quase todos esses anúncios são de 11/11, quando o programa ainda observava os comuns, e a vida medida inclui a espera até a leitura seguinte, então é um teto. Das 13 aprovações registradas de 10 a 14/11 (parte delas pelo modelo mais frouxo da época), 8 tinham preço até 1,1 × o piso de hoje do nome: o achado típico é o seed pagante anunciado pelo preço comum, que é o que a varredura lê. Só que esses anúncios não ficam parados no piso: nas 6 h até 01:55 de 18/11, os 100 mais baratos de 25 nomes lidos pela varredura (2.500 anúncios, todos com seed) teriam por acaso 39 anúncios com seed do alvo (seeds do alvo ÷ 1.001 por anúncio) e tinham 0; nas 77 páginas da varredura no log, só 3 (o mesmo nome, Five-SeveN | Heat Treated (Battle-Scarred), com os mesmos anúncios acima do piso) tinham anúncio pagante. Não medido: se quem tira esses anúncios do piso é o vendedor, que já anuncia caro, ou outro comprador mais rápido.
   Em 18/11/2026, na primeira versão instalada, os primeiros 199 s tiveram 6 tentativas e 1 leitura; outra execução teve 10 tentativas e 5 leituras em 426 s (cerca de uma leitura a cada 85 s, em vez de 60 s).
   Páginas do piso não registram avaliações de anúncios comuns (sem grupo raro) recusados como `sem_lucro` nem dos comuns acima do saldo (`acima_do_saldo`): eram cerca de 4.300 linhas por hora para um limite de 300.000 linhas consultado por 14 dias.
   Nenhum leitor usa essas linhas hoje: a agenda conta só seeds pagantes, a tela do Aprendizado só `aprovada`, `acima_do_saldo_boa` e `zona_amarela`, e a trava de aviso repetido só `acima_do_saldo_boa`.
   Limite aceito: se um seed comum for aprendido como pagante depois, a agenda não conta os anúncios dele que o piso viu e que sumiram antes disso (antes da varredura eles nem eram lidos); os que continuam anunciados voltam a ser avaliados na consulta seguinte, porque a memória de anúncios já avaliados é limpa sempre que um seed entra ou sai dos pagantes.
   O comum sem lucro avaliado no piso também fica sem linha quando uma faixa o reencontra sem mudança de preço, histórico ou concorrente, porque já está avaliado; o comum acima do saldo continua registrado pela consulta de faixa, uma vez por ciclo.
   Nomes com oportunidade ativa têm prioridade após o intervalo mínimo, mesmo com piso acima do saldo.
   A varredura fica fora do registro de observações, e a primeira faixa que reencontra o anúncio recebe
   uma vez o resultado da avaliação no piso (anúncio novo, achado e lucro ou quase-achado).
   Quando vence, a varredura pega no turno da caça a próxima vaga da rota principal, a mesma que a
   consulta por faixa esperaria. O prazo é o intervalo entre leituras mais 3 s de folga, ampliada
   quando o desvio máximo do ritmo ultrapassa 3 s (15,6 s no total hoje). Uma vaga distante é pulada.
   Com R$ 41,91, cerca de 2% do estoque raro das duas famílias Heat Treated cabe no saldo. Dos 30 anúncios raros que cabem, 28 dão prejuízo na estimativa média da bancada; 2 rendem cerca de R$ 9 no cenário médio e dão prejuízo no pessimista. A estimativa da bancada (mediana dos múltiplos das vendas do CSFloat × menor preço Steam do nome) é mais grosseira que a avaliação do programa, que deu +R$ 0,70 no cenário médio para o anúncio de seed 882, abaixo do mínimo de R$ 1,00. O programa ainda não havia avaliado o anúncio de seed 730. As medições não sustentam
   a expectativa de um achado real por hora. A varredura continua porque a consulta por faixa do
   Desert Eagle | Heat Treated raramente traz página e um anúncio novo do seed pagante 730 apareceu
   no piso em cerca de uma hora (medições em [conhecimento.md, seção 1.1](conhecimento.md#11-interface-antiga-e-opção-de-sair-do-beta)).
   **Caça de comum em desconto desligada (11/11, pedido do Erick):** `main` chama `Hunt::disable_cheap_hunt` e a
   seleção fica só com seed e float. Quem anuncia abaixo do maior pedido de compra vende na hora para o pedido, e
   em 6 dias foram 5 235 anúncios de comum avaliados para 1 achado de R$ 0,93. Ela gastava 150 das ~277 consultas
   por hora do app (livro 94, interface antiga 48, busca 8; medido em 11/11, 04:00–10:30Z). Saem junto: os alvos
   `barato` do painel, a exploração de alvo novo e os pedidos de compra sugeridos (leem o livro dos alvos comuns).
   O código continua (os testes ligam a caça). O teto anterior de ~100 consultas/h foi substituído pelo teste limitado do item 3.
   Nenhum desses ritmos foi comprovado como seguro — as 8 janelas livres desde 06/11 (123 a 262 consultas/h de
   anúncios + histórico) terminaram todas em 429 depois de 1 a 7 h, sem relação com o ritmo.
   Em 10/11, ~200 leituras de anúncios/h por 3 h terminaram em 429 desse balde por horas, com o livro respondendo;
   124–153/h tinham rodado 3–4 h sem 429. O número é hipótese de poucas horas. O achado ativo é conferido a cada
   60 s enquanto é novo e depois a cada décimo da idade, até 30 min: um achado parado há 5 h gastava 40 leituras
   de anúncios por hora. **Chaveiro (11/11, pedido do Erick):** todo anúncio lido traz os chaveiros presos à arma;
   o que cada um rende tirado e vendido separado abate do custo do anúncio na avaliação (`detect::charm_gain`), sem
   consulta nova à Steam. Rende = líquido da taxa do Mercado sobre o preço público (`charm_prices`, da mesma
   tabela diária de referência, US$ × razão medida) − uma remoção (US$ 0,33: pacote de 3 por US$ 0,99). Só entra
   chaveiro com venda nos últimos 7 dias (1 856 dos 6 155 da tabela em 11/11), pelo menor preço médio entre 24 h,
   7 e 30 dias, e só se render mais que a remoção; o texto da oportunidade mostra a conta. Anúncio com chaveiro
   contado não vira compra virtual do professor. Medido antes de construir: em 9 419 anúncios vistos (06–11/11),
   179 tinham chaveiro e nenhum rendia R$ 2 depois da remoção — o aviso é para o caso raro, não uma fonte de
   achados. A remoção sai na hora, gasta um removedor e devolve o chaveiro ao inventário (ajuda da Steam,
   "CS2 - Charms"; removedor vem com chaveiro do Arsenal, nos pacotes semanais ou comprado no jogo). O chaveiro
   tirado guarda a trava de 7 dias da arma (informado pelo Erick em 11/11, fontes de terceiros): é a mesma espera
   que a oportunidade já conta para a arma. Não confirmado: o preço da remoção (US$ 0,99 por 3). Adesivo fica de fora: raspado, some; no CSFloat o comprador pagou ~10 % do
   valor de referência dos adesivos (1 365 vendas do cache) e na Steam não há prova de prêmio.
   Venda do CSFloat com chaveiro não é venda limpa (como a com adesivo): o chaveiro paga
   acima da base e não prova pattern. **Bloqueio sem cegueira (10/11):** com a Steam barrando o histórico de
   preços, a avaliação usa o guardado de até 24 h e escreve a idade na evidência; antes, todo candidato de nome com
   histórico vencido (6 h) ficava "adiado" enquanto o bloqueio durasse. Nome sem histórico guardado continua sem
   avaliação. 24 h é hipótese. No painel, achado que passou do prazo de conferência (o de `recheck_secs`, mais
   2 min) aparece apagado, com "Sem conferir há …", e não conta no número da aba: em 10/11 um achado de seed raro
   ficou 3 h na lista de abertos sem que o app pudesse olhá-lo. **Conferência pelo livro (10/11):**
   com os anúncios barrados, o achado de alvo raro é conferido pelo livro de ofertas: `rgCompactSellOrders` lista
   todos os preços à venda (a soma das quantidades fecha com `cSellOrders`; visto em 2 respostas, de 92 e de 1 539
   anúncios), então preço sem degrau a 1 % é anúncio que saiu. Degrau presente não prova nada. A página do anúncio
   (`?detail=`) respondeu 429 sem login no mesmo bloqueio. O teto desses dois filtros não conta o menor anúncio do livro como
   concorrente: ele é o próprio candidato, e derrubaria o teto abaixo de si mesmo. Consultas de anúncios idênticas compartilham a página. O log de ciclo conta consultas de anúncios economizadas. Cada consulta devolve os anúncios mais baratos; `listingid` novo com preço ≤ saldo vai para
   avaliação; anúncio que sumiu vira evento `gone`. Acima do saldo, o anúncio é avaliado uma vez por ciclo só com o que
   já está no banco (nenhum histórico novo nem CSFloat); se seria aprovado, grava `acima_do_saldo_boa` e avisa no
   Telegram com o que falta no saldo, sem virar oportunidade, compra, placar ou crédito da agenda. Raro recusado sem
   lucro com prejuízo médio entre R$ 1 e zero e perda ≤ R$ 0,50 vendido como comum grava `zona_amarela`: candidato
   virtual, sem compra nem aviso; o painel conta os três tipos por hora (24 h) e rotula os virtuais pelo registro
   (sumiu, em até 48 h sem relistagem, relistado), o que não prova venda.
   Medido em 18/11/2026 às 02:40 numa cópia do banco instalado (`over_saldo_on_a_real_db`, sem rede, regras de
   hoje): dos 3.189 anúncios gravados como `acima_do_saldo` em 7 dias, 0 seriam aprovados e 0 avisariam; 2.703 sem
   lucro (3 deles com prejuízo médio de até R$ 1, nenhum na zona amarela), 286 sem histórico, 184 com poucos dias de venda e 16 com poucas
   vendas. Vale só para os anúncios que a caça leu (alvos escolhidos para o saldo de R$ 41,91) e avalia com o banco
   de agora, não com o de cada momento; não mede o que mais saldo traria em skins que a caça não lê.

## Aprendizado (mina de ouro)

Desenho: `docs/superpowers/specs/2026-11-06-mina-de-ouro-design.md`; plano: `docs/superpowers/plans/2026-11-06-mina-de-ouro.md`.
Os números abaixo (60 dias, 24 h, 98 %, 5 degraus) são hipóteses de calibração, não medidas.

1. **Registro de observações (`observations`, `hunt/ledger.rs`).** Toda página de anúncios lida grava cada anúncio
   uma vez por consulta exata (código, desgaste, filtro): `listing_id`, `asset_id` (o item), skin, preço, seed, float,
   paint, grupo raro, primeiro e último instante visto e se era o mais barato da skin na página.
   Na interface antiga, a chave inclui o nome completo: sua cobertura não prova sumiço nem idade de outra versão
   ou de uma consulta nativa. Cada versão é consultada e registrada separadamente.
   - **Idade máxima:** anúncio novo cujo preço a consulta anterior da mesma chave cobria nasceu entre as duas:
     `age_max` = intervalo entre elas. Sem cobertura (preço acima da página anterior, ou 1ª consulta), fica vazio.
   - **Sumiço:** só com página não vazia; some quem não está nela e tem preço coberto (sem `more`, todos; com
     `more`, abaixo de 98 % do maior preço da página, porque a Steam inverte ~1 % na borda). Reaparecer desfaz.
   - **Relistagem:** o mesmo `asset_id` em outro anúncio com o antigo vivo ou sumido há ≤ 24 h marca o antigo
     `relisted` (não é venda) e o novo `relist_of`.
   - Retenção de 60 dias (`db::LEARN_KEEP_DAYS`), como `evaluations` (antes, as últimas 10 000; agora teto de
     300 000 linhas), `book_snapshots` e o placar por hora.
2. **Registro do livro (`book_snapshots`).** Cada leitura do livro de ofertas (pedidos sugeridos ou triagem do
   comum) guarda o maior pedido de compra, os números de pedidos e anúncios e os 5 primeiros degraus de venda
   `[preço, quantidade]`, só quando maior pedido, número de anúncios ou degraus mudaram.
3. **Placar (`GET /api/learning`, seção "Aprendizado" do painel).** Por dia de Brasília, 14 dias: leituras de
   anúncios e do livro (`hourly_activity.queries`/`books`), anúncios novos e a idade mediana dos de idade conhecida,
   os que sumiram sem relistagem e quanto duraram (mediana), relistados, quase-achados e achados. Também o resumo
   do professor (item 4).
4. **Professor grátis (`ghosts`, `csfloat_sales`, `hunt/teacher.rs`).** Sem comprar nada e sem gastar o balde de
   anúncios: todo anúncio novo avaliado que virou aviso, ou cujo lucro médio ficou a até `max(R$ 0,30, 10 % do
   preço)` abaixo da régua (quase-achado), vira uma compra virtual a preço do anúncio, com o preço médio previsto.
   De hora em hora o professor rotula:
   - **Rápido (48 h):** comum = parte das compras do histórico nas 48 h com mediana ≥ o previsto, a mediana
     ponderada delas e anúncios da mesma skin a ≥ o previsto que sumiram sem relistagem; "fechou" = um sumiço, ou
     ≥ 50 % das compras; sem compra nem sumiço, indefinido. Raro = sumiço do mesmo grupo a ≥ o previsto ou venda do
     CSFloat do grupo (guardadas em `csfloat_sales` a cada leitura) a ≥ o múltiplo sugerido (previsto ÷ preço comum).
   - **Final do comum (dia 10):** venda virtual pelo menor entre a mediana dos dias 7–9 (a trava da Steam) e o menor
     anúncio do 1º registro do livro nesses dias − 1 centavo; lucro = líquido − preço. Nos dias 7–9 o professor pede
     uma leitura do livro (balde do livro) das skins sem registro. Sem nenhum dos dois até o dia 12: `sem_rotulo`.
   - **Final do raro (dia 14):** fechou entre os dias 7 e 14 pelas mesmas fontes do rápido; sem fechar, vende pela
     mediana comum dos dias 7–9.
   - O placar mostra: compras virtuais, avisos e quase-achados que venderiam em 48 h, real ÷ previsto (mediana, só
     comuns; abaixo de 1 = o app prevê alto), lucro virtual realizado dos avisos contra o previsto e raros que fecharam.
   - Prazos e régua são hipóteses de calibração. Tudo fica 60 dias.
5. **Selo do raro (`pricing/premium.rs`).** Todo raro que vira aviso leva um selo; a regra do aviso não muda.
   - Vendas fechadas do grupo: as do CSFloat guardadas (`csfloat_sales`) e os anúncios da mesma skin no grupo que
     sumiram em até 48 h sem relistagem (múltiplo = preço ÷ preço médio comum da skin, sem prêmio). Valem os mesmos
     dois filtros da prova da Steam (13/11, abaixo): seed de nota melhor não prova o pior, e sumiço com anúncio mais
     barato do mesmo seed à venda não é venda.
   - Prêmio = média geométrica encolhida seed → grupo → 1× com 5 vendas fictícias por nível; p25/p75 das vendas da
     seed quando ela tem ≥ 10, senão das do grupo.
   - "Comprovado" = ≥ 10 vendas fechadas e vender a `base × p25` ainda dá o lucro e a margem mínimos do aviso
     (R$ 1,00 e 10 %). Senão, "ainda não comprovado" com o motivo (quantas vendas, ou quanto dá o pior quartil).
   - Preço sugerido: anunciar a `base × p75` e descer até `base × prêmio`. Anúncios do grupo à venda há mais de
     48 h (vistos nas últimas 6 h) aparecem como "parados".
   - Sai no painel (etiqueta ao lado do grupo e linha nos detalhes), no aviso do Windows e no Telegram.
6. **Valor de uma leitura (`hunt/value.rs`, refeito em 12/11).** Projeto e medidas:
   `docs/superpowers/specs/2026-11-12-so-pattern-design.md` (o de 10/11,
   `2026-11-10-aprendizado-unificado-design.md`, descreve a conta por alvo que esta substitui).
   - **Unidade = faixa de seed.** Uma consulta de anúncios filtra uma faixa de seeds de uma skin × desgaste. A
     agenda compara faixas pelo índice marginal `I = v * W * (1 - exp(-x) - x exp(-x))`,
     com `v = taxa * peso * lucro` e `x = gap/W`. Para esperas pequenas cresce como
     `v * gap^2/(2W)`: frequência próxima da raiz do valor relativo. Hipótese de alocação,
     sem ganho real medido. Toda leitura custa uma consulta de
     anúncios (medido em 13/11: 1,00 a 1,03 por leitura), então dividir pelo custo não muda a ordem.
     - `peso` = seeds pagantes da faixa × vendas/dia da skin, normalizado para média 1 entre as faixas
       selecionadas: quanto de anúncio novo a faixa deve receber;
     - `Δ` = segundos desde a última leitura **da faixa** (`progress.seen`, sobrevive a reinício); `W` = a janela
       da `taxa` (abaixo). **Estoque da faixa (13/11):** era `L` = percentil 25 da vida dos anúncios mais baratos
       (792 s), que vale para anúncio comum. Medido em 20 h de leituras de faixa: anúncio novo ≤ saldo por leitura
       foi 0,004 com a faixa relida a < 5 min e 0,18 a 1–4 h, e a agenda relia as mesmas 2 faixas a cada 2–3 min
       (1 294 de 2 073 leituras, 5 anúncios novos). Com a caça barata desligada, o ajuste de `W` usa só as
       releituras de faixa (escolheu 28 800 s, o topo da grade). Ensaio anterior de 7 dias × 10 sorteios (da mudança de estoque, não da agenda marginal)
       (`hunt/agenda_sim.rs`, premissas lá): anúncios novos por leitura iguais ou maiores que a conta antiga em 6
       cenários (vida 15 min a 12 h × mercado observado/giro), de 0 a +54 %;
     - `lucro` = líquido de vender pelo piso comum × mediana da prova dos seeds da faixa − o piso comum, em
       centavos (mínimo 1);
     - `taxa` = anúncios novos dentro do saldo por segundo e por peso, medida no alvo (skin × desgaste):
       `a = r + m·W·λ`, `taxa = (a + √a) ÷ (exposição + m·W)`. A exposição de cada leitura é
       `peso × W·(1 − e^(−Δ/W))` da faixa lida, gravada com a leitura (`reads.gap`, `reads.weight`); `λ` é a taxa
       da classe, onde o alvo novo começa; `√a` é o otimismo de contagem, que some com medida; `r` e exposição
       pesam metade a cada `H`.
   - **Agenda marginal (16/11):** muda apenas a escolha; exposure, taxa aprendida, registro e
     controle 1/8 continuam iguais. Sem novo ajuste ou migração. O controle limita a espera
     em escolhas elegíveis de `next_matching` (no teste, só modo seed); não garante tempo de parede:
     `overdue_task` tem prioridade e pode ocupar vagas. `recheck_secs` continua entre 60 e
     1800 s, um décimo da idade do aviso; a troca não altera reconferências.
     Na cópia sanitizada, os 105 snapshots entre 01h08min11s e 01h57min50s de 16/11/2026 (Brasília) incluem
     92 escolhas da agenda e 13 do controle. As 51 esperas <600 s (38 <300 s) são da
     agenda; nenhuma reconferência nesse recorte. Cinco faixas receberam 53/92 escolhas
     da agenda (57,61%). Antes dos snapshots não há faixa identificada: não extrapolar.
     Simulador usa `Params::default`, SALDO=3608 e RATIO=5.5 explicitamente como
     hipóteses diagnósticas, nunca saldo atual ou câmbio recuperado. Não lê settings.
     Teste determinístico com 227 faixas, uma valendo 25 vezes as outras: frequência
     relativa caiu de 24,71 (regra antiga, teste falha) para
     6,15 / 5,18 / 4,90 (marginal, 10 mil escolhas por janela de 2 h / 8 h / quase infinita), perto de sqrt(25)=5;
     maiores esperas 233 / 232 / 231 escolhas elegíveis, abaixo do limite de controle 8*227=1816.
     Horizonte reduzido: 2 dias, 3 sorteios iguais por regra; observado tem viés das
     faixas já lidas; giro presume distribuição uniforme de seeds; chegada Poisson e
     vida exponencial são hipóteses. Começa sem aprendizado; estoque inicial pode contar
     como captura na simulação, embora não ensine nem entre no placar real.
     Comparação offline das duas regras, mesmos sorteios, 12 cenários: capturas
     de +0.00% a +43.48%; valor estimado de -13.59% a +92.66%.
     Todas cobriram 227 faixas por causa do controle; isso não prova espalhamento.
     O simulador mede também a fração das escolhas da agenda nas cinco faixas mais frequentes
     (exclui controle; soma as cinco mais frequentes de cada sorteio) e a maior espera
     por faixa, incluindo o começo e o fim do horizonte. Não reproduz necessariamente a concentração real.
     Capturas não pioraram, mas houve perda de valor simulado em alguns cenários; o modelo não prova ganho real.
     Ensaio release: compilação em 4 min 03 s; execução em 60,07 s para stock e 49,58 s para marginal.
   - **Chance de compra por skin (13/11):** o `lucro` da faixa é multiplicado por (bons + 1) ÷ (vistos + 10): vistos =
     anúncios de seed pagante da skin avaliados em 14 dias (inclusive acima do saldo), bons = os que deram lucro no
     preço médio ou viraram aviso (`catalog::load_deals`). Medido em 12/11 (6,6 dias): Heat Treated da Desert Eagle
     0 em 847 (quem anuncia seed de azul já pede preço de azul), Five-SeveN Heat Treated 0 em 339, Serpent Strike 5
     em 41, MP7 Amberline 4 em 81. Antes, a conta supunha todo anúncio novo comprado no piso comum e a agenda pôs
     82 % das leituras no Heat Treated. O prior 1/10 é hipótese.
   - **Peso medido (13/11):** o `peso` de cada seed passa a ser os anúncios dele vistos em 14 dias + vendas/dia × 14 ÷
     1 001 (prior de giro), no lugar de seeds × giro. Medido: Serpent Strike (2 seeds, ~450 vendas/dia) mostrou 41
     anúncios em 6,6 dias e MP7 Amberline (5 seeds) 81; com seeds × giro a agenda pôs 90 de 123 leituras de uma hora
     no Serpent Strike, sem nenhum anúncio novo.
   - **Teto por seca:** sem anúncio novo em `s` de exposição seguidos, a taxa não passa de `3 ÷ s`.
   - **Primeira leitura da faixa não ensina (12/11, depois de 2,5 h rodando):** tudo o que está à venda numa faixa
     nunca lida é "novo" para o app. Medido: 10 "novos" na 1ª leitura do Solitude e 0 nas 25 seguintes; os 4
     "novos" do rodízio cego eram todos 1ª leitura, e por isso ele parecia render 3× a agenda. Agora essa leitura
     tem exposição e recompensa zero, e a repetição e o placar a pulam (`value::range_exposure`).
   - **Seleção:** até 24 alvos raros, ordenados por Σ `peso × lucro` das faixas.
   - **Três conferências contra placebo**, gravadas de hora em hora em `settings.value_info` e mostradas na seção
     "Aprendizado" do painel:
     1. **Controle.** 1 leitura em 8 (`EXPLORE_SHARE` = 12,5 %) ignora o valor e lê a faixa lida há mais tempo.
        Toda leitura grava `worth` = anúncios novos × lucro da faixa e `gain` (lucro previsto dos avisos); o
        placar "Agenda × rodízio cego (7 dias)" compara o que cada braço rende por leitura. Se a agenda não
        ganhar do rodízio, o aprendizado não está servindo. Durante os primeiros sete dias após
        a troca, esse placar mistura escolhas da regra antiga e da nova, até as antigas saírem
        da janela; o registro existente não foi alterado. O painel também compara, por faixa de
        espera, somando as faixas de seed, a média de anúncios raros novos no saldo por leitura; mostra as contagens até haver 30 por braço.
     2. **Previsão.** O app repete as leituras dos últimos 7 dias (`reads`; páginas do mesmo alvo em até 90 s
        contam como uma) prevendo cada uma só com as anteriores, para `W` {0,5; 1; 2; 4; 8 h} × `m` {0,5; 1; 2;
        4; 8; 16} × `H` {24; 72; 168 h}, e mede o erro (perda de Poisson). Troca os parâmetros só com ≥ 1 000 leituras
        pontuadas e melhora > 1 %; a troca vai para `refit_log` (`fluxo:*`).
     3. **Preço.** A prova retroativa do catálogo ("Precificação", "Catálogo de pattern").
   - **Migração:** a exposição mudou de unidade, então `r`/exposição dos alvos raros e as leituras antigas de raro
     foram zerados; os alvos de float saíram. O ajuste de parâmetros só volta a trocar depois de ≥ 1 000 leituras
     novas (~8 h de caça).
   - **Hipóteses, não medidas:** a vida do anúncio raro barato (9 sumiços em 215 vistos); `W` = 2 h, `m` = 1, `H` = 72 h (medidos em 10/11
     na conta antiga, por alvo); `Z` = 1; o teto de 3; `worth` como sinal de lucro (o app teve 5 oportunidades em
     toda a vida, então lucro realizado não dá para comparar braços); o lucro usa razão do CSFloat.
   - **Vieses conhecidos do placar:** o rodízio lê faixas com mais tempo parado, então acha mais estoque por
     leitura do que acharia lendo no ritmo da agenda (comparação conservadora para a agenda); o peso médio muda
     quando o catálogo muda.
7. **Recalibração (`teacher::refit`).** Uma vez por dia de Brasília, com ≥ 7 dias de rótulos rápidos e ≥ 30 rótulos de
   comum com mediana na última semana: alvo = fator aplicado × mediana de (real das 48 h ÷ previsto), em
   [0,80; 1,00] (só desconta), andando no máximo 2 % por dia. Entra como `min` com o fator das vendas do Erick no
   desconto do comum (raro não muda); a frase de confiança diz qual dos dois descontou. Toda mudança vai para
   `refit_log` e para o log. O placar traz o fator e o boletim da semana contra a anterior (consultas, achados,
   quase, avisos que venderiam em 48 h, lucro virtual).

## Exploração fora dos favoritos

Plano e condições: `docs/superpowers/plans/2026-11-06-ml-exploracao-final.md`. O núcleo fica em
`research.rs` (custo/cota), `hunt/explore.rs` (candidatas) e `hunt/trial.rs` (comparação persistente).

- A nova versão registra a política vigente e coleta pelo menos 24 h e 100 tentativas físicas. Depois
  alterna oito pares de blocos de 24 h, balanceados por semente persistida; as primeiras 6 h são transição
  fora da comparação. Reiniciar não reinicia o protocolo. Números são hipóteses, não calibração de mercado.
- No piloto, uma das 40 vagas comuns recebe uma candidata elegível fora do ranking, com histórico fresco.
  Rodízio por nunca testada/mais antiga e família; consultas somente em parte dos slots de exploração
  comum. Controle permite que a mesma skin entre normalmente no ranking. Não altera a régua de compra.
- Custo por tentativa, incluindo histórico/repetição/saída extra/fallback, atribuído à decisão original.
  Pedido compartilhado custa uma vez; busca normal não puxa a candidata para avaliação adicional fora da
  cota. Janela móvel de 1 h: máximo 6 tentativas externas, pelo menos 15 normais por externa e máximo 3 por
  decisão. Reserva persistida/atômica antes do transporte, sempre dentro do limitador. São hipóteses.
- Erro certo antes do envio vira `not_sent` e não entra na cota. Reserva sem desfecho após queda do processo
  continua `started`, custo conservador que não prova recebimento pela Steam. CSFloat tem contagem própria;
  compra/login/sessão não herdam cota de exploração. Intervalos são reconferidos no envio, após o banco.
- Visitas espaçadas por 1 h. Após 6 h e três visitas, ou 24 h sem terminar, a vaga gira. Retenção provisória
  por mais uma janela exige três aprovados únicos, 12 tentativas da candidata e 12 de referência nas últimas
  24 h, com rendimento aprovado/tentativa superior ao comum normal. Não promove por quase-achado. Limiar
  hipotético, sem alegar superioridade estatística individual; depois a vaga volta a desafiar outra skin.
- Ativo impede rotação que o abandone; conferência atrasada há 10 min precede pesquisa/descoberta dentro
  do limitador. Acompanhamento usa vagas normais, entra no custo total e aparece separado. Instalação que
  herdou mais ativos que o teto mantém esses acompanhamentos, sem abrir candidata extra.
- Tarefas, identidade/visitas da candidata, crédito, rodízio de modos e cursor de seed são persistidos.
  Históricos ausentes ou mais antigos vêm antes dos mais populares. Falhas ainda dependem do rodízio em
  memória daquela descoberta para não repetir no ciclo; não há nova consulta fora da cota para isso.
- Achados e créditos são deduplicados por ID por 60 dias, entre tarefas/reinícios. Achado e oportunidade
  são gravados na mesma transação. Identificação de relistagem continua limitada à cobertura observada.
- Professor mantém decisão original e coortes por compra virtual. Raro sem sinal fica sem rótulo;
  evidência atrasada pode preencher o resultado e relistagem desfaz o indício. Amostras externas recusadas
  ficam fora da recalibração/placar padrão. Painel separa previsto, virtual, pendente, sem rótulo e maduro;
  vendas reais continuam no registro de compras, sem serem inferidas do virtual.
- 429 exploratório ou limite violado interrompe persistentemente o ensaio e restaura a política base.
  O ajuste horário da agenda (item 6 do Aprendizado) segue rodando durante a comparação. O relatório por blocos mostra
  diferença/incerteza, mas não elege vencedor enquanto maturação, cobertura e memória não forem comprovadas.
- Pesquisa dura até 60 dias, teto nominal de 500 mil linhas por tabela; últimos 14 dias e vínculos de
  compras virtuais são preservados mesmo acima do teto. Medir espaço antes de aumentar tráfego; retenção
  nunca apaga compras para abrir lugar. Alvos e deduplicação também expiram com o registro.

As ampliações para cortes anteriores de descoberta, exploração externa de seed/float, recusados distantes
e compartilhamento de aprendizado dependem da evidência do piloto e não são liberadas por calendário.
Esta versão prepara coleta/comparação; não comprova mais lucro. Seed/float internos continuam com rodízio.

## Precificação (o coração)

- Taxas: fórmula em centavos de `conhecimento.md` 5.1; inverso por busca binária. Nunca ÷ 1,15.
- A skin comprada fica **7 dias travada**: todo preço de venda é para daqui a 7 dias.
- Base (item comum): pontos horários dos últimos 7 dias de `QueryPriceHistory`, quantis ponderados
  por compras: Q25, Q50, Q75. Tendência: mediana dos últimos 3 dias contra a dos dias 4–14; se cair,
  multiplica tudo pelo fator projetado em 7 dias (piso 0,85); se subir, não credita (fator 1).
  Sem estimativa com menos de 3 compras nos 7 dias ou com compras em menos de 3 dias distintos (pico ou lote velho).
- `quick = min(Q25·f, menor outro anúncio − 1 centavo)`, `mid = min(Q50·f, menor outro anúncio − 1 centavo)`, `high = Q75·f`
  (quem compra leva o mais barato: vender acima do concorrente já à venda não é o mais provável).
  Concorrente = mesmo nome e mesmo grupo raro (ou ambos comuns); no raro conta também o menor anúncio do grupo
  visto nas outras faixas de seed da mesma skin nas últimas 6 h, porque a página de uma faixa não mostra as outras.
  No grupo de float só concorre o anúncio de float igual ou melhor: float pior mais barato não segura a venda.
- Float de elite: **removido em 12/11** (pedido do Erick). Float não cria grupo, alvo nem prêmio; `floats.json`
  só serve para tirar da prova de pattern a venda cujo float está nos 10 % melhores do desgaste (o prêmio dela
  pode ser do float). Medidas que ficam de histórico: na Steam, de 05 a 11/11,
  anúncio de float bom sumiu sem voltar em 36 % dos casos a 1,15–1,5× o comum e 5 % a 3× ou mais; 205
  fechamentos de Red Quartz na faixa saíram a 0,99×.
- **Catálogo de pattern (`src/hunt/patterns.rs`, 12/11).** As tabelas (`bluegem`, `cspatterns`, `fade`, `phases`,
  `named.json` com 28 grupos nomeados em 22 skins, `blue.json` com o % de azul por seed de 5 skins) só indicam
  candidatos. Quem decide se um seed é raro são as vendas guardadas do CSFloat (`csfloat_sales`, sem adesivo nem
  chaveiro). Cada venda vira uma razão sobre o preço comum do nome (a mediana das vendas dele, com ≥ 10 vendas
  limpas). O seed paga quando:
  1. a skin tem nota por seed (% de azul ou % do fade): as 8 vendas de nota igual ou pior mais próximas da dele
     têm mediana ≥ 1,5× e quartil de baixo ≥ 1,2×. Seed melhor herda a prova do pior;
  2. é seed de tabela com 3+ vendas próprias: as vendas dele passam no mesmo teste;
  3. é seed de tabela com menos de 3 vendas: as vendas do grupo dele passam com quartil de baixo ≥ 1,5×. O grupo
     prova que o seed paga, não quanto: cada razão da prova fica limitada à pior venda própria do seed ou, se ele
     nunca vendeu, à pior venda do grupo (18/11). O grupo de tabela junta seeds de tiers diferentes e a mediana
     dele é puxada pelos melhores: em 73.310 vendas guardadas, nas 27 de seed com prova emprestada a venda real
     saiu a 0,74× o previsto (mediana) e 12 saíram abaixo da metade do previsto; com o limite, 1,14× e 2. Caso que
     mostrou: Glock-18 | Grinder #593 Minimal Wear, previsto a 17,2× pelo grupo "Full Black" (que tem o #916,
     vendido a 28,9×) e vendido a 4,6×. Nessa conta o preço comum de cada nome é a mediana de todas as vendas
     guardadas dele, inclusive as posteriores à venda prevista. O aviso de seed com prova emprestada diz que o
     prêmio é um teto, sai com risco alto e confiança no máximo média: as vendas contadas são de outros seeds.
     Quando a prova de um seed muda sem ele entrar nem sair (venda nova, teto novo), a oportunidade ativa dele
     volta a ser avaliada na próxima consulta, para o lucro na tela não ficar com a prova antiga.
     Três ou mais fechamentos na Steam continuam substituindo a prova (e o teto): nos grupos nomeados são só os
     do próprio seed, porque a chave do grupo é por seed; em Glock-18 | Moonrise e High Beam, os do mesmo tier do
     guia. Custo medido (18/11, 73.645 vendas guardadas): nas 7 primeiras vendas de seed de tabela que nunca
     tinha vendido, a venda real saiu a 1,9× o pior do grupo (mediana; a menor a 0,8×, 3 acima do dobro). O
     quartil de baixo do grupo centra melhor (0,83×) mas previu acima do real em 4 das 7 (0,42×, 0,56×, 0,73× e
     0,83×): o teto fica no pior, porque errar para cima é comprar caro. Separar os grupos por tier não resolve:
     dentro do mesmo grupo a mediana por seed varia 11× na Grinder "Full Black", 35× na MAC-10 | Snow Splash
     "Max White" e 54× na Galil AR | Sandstorm "Max Purple", e o guia da Grinder põe o #593 no mesmo tier do #916
     (`docs/pesquisa-web.md`, seção 5). Só a venda do próprio seed diz quanto ele paga.
     A mesma conta por regra (`catalog_on_a_real_db`, venda real ÷ prevista, mediana): regra 1, 558 vendas a
     1,04× (56 abaixo da metade do previsto); regra 2, 71 a 1,15× (4); regra 3, 27 a 1,14× (2); regra 4, 7 a
     0,84–0,95× (1). Na regra 4 em skin com piso abaixo de US$ 0,50 não há venda posterior para conferir: não
     medido. Fechamentos na Steam por tier do guia: 42 sumiços, cada um previsto pela mediana dos outros do
     mesmo nome e tier (sem o filtro de nota), saíram a 1,05× (3 abaixo da metade, 6 acima do dobro);
  4. não está em tabela nenhuma: 3+ vendas próprias com o quartil de baixo a ≥ 2×.
  O quartil de baixo arredonda para baixo: com 3 ou 4 vendas é a pior delas. O seed pagante sem grupo de tabela
  vira `learned:<skin>:<seed>` ("Pattern #seed"); o prêmio são as razões da prova.
  - **Prova por desgaste (13/11).** Nas regras 2 a 4 (skin sem nota) só contam vendas no desgaste do anúncio ou num
    pior: o seed pode ser raro no Factory New e comum no Battle-Scarred. Medido nas vendas guardadas, em 371 pares
    do mesmo seed que pagava ≥ 1,5× no desgaste melhor: sem nota, 44 de 221 ainda pagavam ≥ 1,5× no pior; com nota,
    126 de 150 (por isso a regra 1 continua valendo em todos os desgastes). O caso que mostrou: MAC-10 Snow Splash
    #309 Battle-Scarred a R$ 0,34 previsto a R$ 11,84 por três vendas Factory New, com 19 anúncios do mesmo seed em
    desgaste melhor parados a partir de R$ 0,75. Segunda trava, para todo raro: o preço rápido e o médio não passam
    do menor anúncio vivo do mesmo grupo num desgaste melhor do mesmo item (o desgaste pior saiu mais barato ou
    igual em 363 dos 371 pares). No banco de 13/11 a prova retroativa foi de 369 para 353 vendas (98,4 % → 98,6 %
    pagando). Ressalva: os pares têm viés de seleção (só entra quem pagou bem no desgaste melhor).
  - **Compra até a venda (13/11).** "Comprei" na linha da oportunidade (ou "Fui eu que comprei" numa perdida)
    grava a compra com a previsão daquele momento; a aba Compras mostra a trava, o valor de hoje (a mesma conta da
    caça só com o banco: histórico guardado, prova por desgaste e anúncios vivos do registro), o gráfico de 30 dias
    e, depois da trava, o campo "Vendi por". A oportunidade comprada não é apagada pela limpeza de 7 dias. A data
    da trava é estimada (7 dias arredondados para a hora cheia, regra tirada de uma compra só); confirmação pelo
    inventário não existe, porque o app não tem login.
  - **Coletor:** lê as vendas do CSFloat de uma skin a cada 2 min, alternando as skins de tabela (mesmo lentas ou
    caras) e as de mais giro do catálogo (≥ 5 vendas/dia, piso ≤ saldo): a vez é da lista atendida há mais tempo
    (nome das duas conta na de tabela); cada skin das listas no máximo 1×/24 h (a vigiada relê quando o cache de 6
    h vence), sem gastar consulta da Steam. Entre os nomes vencidos lê primeiro o nunca lido e depois o lido há
    mais tempo, dentro da lista da vez (a outra só entra quando a da vez não tem nome vencido), e ao reiniciar
    retoma a volta gravada em `pattern_seen`, que só anota o nome que o CSFloat respondeu (pausa e cota devolvem a
    vez na hora; rede fora do ar adia o nome só até reiniciar): pela ordem de giro ele relia o topo todo dia e não
    chegava ao fim da lista (17/11: só os 124 primeiros dos 338 nomes de tabela; 940 nomes ao todo, mais do que as
    vagas de um dia). A mesma vaga prioriza nomes vigiados cujo cache de vendas falta ou venceu (6 h), a cada 15 s
    enquanto a última consulta não falhou. A cota e a pausa do CSFloat continuam obrigatórias; falha de um nome
    vigiado espera 10 min, e a vaga seguinte à falha de um vigiado é das listas (se elas têm nome vencido), para
    que vigiados falhando sem parar não tomem todas as vagas. Cada leitura refaz a prova; quando os seeds pagantes
    de uma skin crescem, a seleção é refeita na hora.
  - **Medido em 12/11** (45 759 vendas guardadas; teste `catalog_on_a_real_db`, só leitura): 15 skins e 231
    seeds pagantes. Por piso comum: até US$ 2 — Glock-18 High Beam (4 seeds), Sawed-Off Amber Fade (8), USP-S
    Tropical Breeze (1); US$ 3–5 — P2000 Amber Fade (5), Desert Eagle Serpent Strike (2), MP7 Amberline (5),
    SSG 08 Acid Fade (34); US$ 6–9 — Glock-18 Moonrise (5), Desert Eagle Heat Treated (104), M4A1-S Solitude (2),
    XM1014 Seasons (1), AUG Amber Fade (6); US$ 12–15 — Five-SeveN Heat Treated (51), AWP PAW (2), AWP
    Exothermic (1). Saíram seeds de tier 1 que vendem a preço comum (Solitude 95/112/154, Trace Lock 309/390).
  - **Prova retroativa (`patterns::backtest`):** cada venda é prevista só com as vendas anteriores a ela. Em 12/11:
    336 vendas de seed que já pagava; 97 % saíram a ≥ 1,15× o comum; real ÷ previsto 1,12 (mediana). Os limites
    (1,5 / 1,2 / 2,0, 8 vizinhas, 3 vendas) foram escolhidos olhando esse mesmo retrato, então o número é
    otimista; o placar refaz a conta a cada hora com as vendas novas, que a regra não viu.
  - **Limites:** razão do CSFloat não é preço da Steam (medido em 11/11: Center Star 1,6× lá × 2,0–2,6× aqui;
    High Beam T1 2,9× × 1,4×); skin com menos de 10 vendas limpas por nome não é medida e não é caçada; facas e
    luvas ficam fora; a tabela `learned_sales` fica no banco, sem uso.
- Raro: o grupo do anúncio (tier, fase, faixa de fade, float de elite) busca vendas do CSFloat do mesmo grupo
  (`price / reference.base_price`) entre todas as guardadas (`csfloat_sales`), não só as da última leitura do
  nome: as do grupo no mesmo nome ou, com menos de 8, as do grupo em todos os desgastes. Em 10/11 um Moonrise FT
  Center Star a R$ 7,20 (1,2× o comum) foi recusado com "5 vendas, 1,4x a 1,6x" quando o banco guardava 39 vendas
  do grupo (mediana 1,78×). Com n ≥ 3: `mid` e `high` da base × Q50/Q75 do múltiplo; `quick` fica sem
  prêmio (× min(1, Q25)), porque o prêmio do CSFloat já saiu abaixo na Steam (`conhecimento.md` 5.6). Com n < 3:
  usa a base comum e escreve na evidência "prêmio sem prova (n vendas)". Preço pedido nunca vira preço.
  **Prova da própria Steam (11/11):** em grupo de tabela ou seed aprendido, quando o banco guarda 3 ou mais
  anúncios do mesmo nome e grupo que sumiram em até 48 h sem relistagem (`observations`), o múltiplo passa a ser
  preço ÷ preço comum desses sumiços, no lugar do CSFloat (`detect::steam_closed`), e a evidência diz "prêmio
  medido na Steam". Medido em 11/11: Center Star FT 1,6× no CSFloat × 2,0–2,6× fechado na Steam; High Beam T1
  2,9× × 1,4×; seed 425 do Serpent Strike 1,0× na Steam. Refeitas as avaliações antigas de raro com 3+
  fechamentos, a regra muda uma (o Moonrise a R$ 7,20 recusado em 10/11 passa a +R$ 3,72) e não cria aviso. Grupo de
  float fica de fora: os anúncios da faixa são pedidos a preço comum e somem como comum (205 fechamentos de Red
  Quartz a 0,99×), a mediana não mede o prêmio. Limites: sumir não é vender com certeza e anúncio barato some
  primeiro (a mediana tende para baixo); 3 fechamentos e 48 h são hipóteses.
  **Dois filtros no sumiço (13/11):** o caso foi o SSG 08 Acid Fade #690 a R$ 19,46 previsto a R$ 47,04 (10,6× o
  comum) por 8 sumiços do grupo "Acid Fade 99%+", todos de outros seeds.
  (1) Em skin com nota (% de azul, % do fade) o sumiço e a venda do selo só provam seed de nota igual ou melhor
  que a do seed vendido (`detect::proves`): dentro do grupo o prêmio cai com a nota. Vendas do CSFloat do Acid Fade:
  rank 1 a 27× o comum, rank 2 a 10,6×, ranks 3 e 4 a 5,4–6,6×, ranks 9 a 17 a ~3×; o #690 é o rank 14 e as duas
  vendas dele saíram a 2,3× e 3,4×. Os 8 sumiços eram dos ranks 2 a 5.
  (2) Sumiço não conta quando um anúncio mais barato do mesmo nome e seed estava à venda junto e continuou depois
  (`teacher::NO_CHEAPER_TWIN`, também no rótulo das compras virtuais): quem compra leva o mais barato. No banco de
  13/11, 19 dos 75 sumiços em grupo de pattern eram assim (R$ 163,08 "vendido" com o mesmo seed parado a
  R$ 59,88); no Acid Fade 99%+ sobram 2 dos 8, e 5 dos 14 pares nome+grupo com 3 ou mais sumiços ficam com menos
  de 3 e voltam para a prova do CSFloat. Ressalva: o `grp` gravado em `observations` é o da hora em que o anúncio
  foi visto; sumiço antigo guardado com outra chave de grupo não entra na prova nova.
- Prazo `eta_hours` = 24 / (vendas/dia × fração do peso com mediana ≥ preço), depois da trava.
- Confiança: alta (≥ 20 vendas/dia e ≥ 100 pontos em 7 dias; raro com n ≥ 8), média (≥ 5/dia; raro n ≥ 3),
  baixa no resto.
- Nota por dia é uma hipótese, não probabilidade medida de venda: em cada preço, calcula a fração do peso
  dos últimos 7 dias com preço projetado igual ou maior; no raro comprovado, combina cada ponto com cada
  múltiplo válido do grupo. Normaliza as três frações para somarem 1 e faz a média ponderada dos lucros
  líquidos. Desconta `preço de compra × min(1, movimento de 7 dias)`: penalidade conservadora para possível
  queda durante a trava, não chance estatística de queda. Arredonda o resultado em centavos.
  Movimento de 7 dias é o desvio padrão populacional das variações entre medianas diárias com 7 dias de
  distância, se houver pelo menos 8 pares nos últimos 30 dias; senão, volatilidade × √7. A mediana diária
  oscila e volta, e o diário × √7 chegou a dobrar o risco medido direto nos itens conferidos. A queda média de
  7 dias entra junto (raiz da soma dos quadrados), para que uma descida contínua não pareça risco zero; no
  caso sem pares, entra a queda média diária × 7.
  Volatilidade é o desvio padrão populacional das variações entre medianas diárias ponderadas por compras,
  em dias consecutivos dos últimos 30 dias; exige pelo menos duas variações.
  Sem nenhuma das duas medidas, lucro esperado é nulo.
  Dias presos = 7 + prazo médio em horas / 24; sem prazo medido (incluindo raro comprovado), dias e nota são
  nulos. Nota = lucro esperado / dias presos, arredondada em centavos; nulos ficam por último.
- Risco baixo: volatilidade ≤ 2 % ao dia, ≥ 20 vendas/dia e, em raro, n ≥ 8. Médio: volatilidade ≤ 5 %,
  ≥ 5 vendas/dia e, em raro, n ≥ 3. Alto nos demais casos, inclusive sem volatilidade suficiente. Esses
  limites são hipóteses para priorização, não calibração com vendas reais.
- Combinação: usa lucro esperado ajustado ao risco positivo; sem estimativa ou com resultado ≤ 0, o item não entra. Se a de maior lucro tiver uma única skin, prefere duas ou três de nomes distintos quando
  o lucro delas alcançar pelo menos 90 % do ótimo e couber no saldo.
- Entra na lista: lucro no `mid` ≥ R$ 1,00, margem ≥ 10 % e lucro esperado ajustado ao risco ≥ 0 quando medido
  (decisão do Erick em 06/11: R$ 0,30 por uma semana travada não vale o risco). O risco aparece na linha.
  `safe` quando lucro no `quick` > 0. Combinação e aviso exigem lucro esperado ajustado ao risco positivo.
  Aviso no Windows (toast, sai com o painel fechado; clicar abre o anúncio), Telegram e som: lucro esperado ajustado ao risco positivo e lucro no `mid` ≥ `notify_min_profit_cents` (padrão R$ 1,00). Não exige `safe`: raro quase nunca é `safe`, e um flip de R$ 10,75 escapou sem aviso por isso.
  O aviso começa assim que o anúncio passa, antes dos históricos dos próximos anúncios. Também sai se a mesma
  oportunidade só atingir o mínimo depois. `notified_at` persiste a entrega aceita pelo Windows ou Telegram;
  falha não confirma envio. `notify_pending_at` reserva por 60 s e permite retomar após reinício. Windows tem
  prazo de 10 s; Telegram, incluindo fila e repetições de rede, 40 s. A finalização só altera a própria reserva.
- Calibração: cada compra grava os três preços previstos; quando o Erick vender, o realizado entra numa
  tabela para conferir o erro (v1 só grava; o ajuste fino vem depois com dados).
  Conferida em 06/11 contra o mercado 7–9 dias depois (7 525 casos, 493 skins): real ÷ previsto no médio com
  mediana 1,000 (p10 0,914, p90 1,104), 50 % do volume ≥ médio e 70 % ≥ rápido, erro médio 6,6 %.
- Venda manual aceita preço entre 1 e 10.000.000 centavos, depois da liberação, e permite corrigir o valor. O valor é o
  preço pago pelo comprador, incluindo taxas, igual aos três preços previstos. `mean_error_pct` é a média do
  erro absoluto e `bias_pct` a média do erro com sinal, ambos `(realizado − médio previsto) / médio previsto`.
  Agrupa pelo nome sem o desgaste e pelo tipo; compras antigas sem tipo nem oportunidade correspondente ficam
  fora da calibração. Horários somam os últimos 30 dias por hora de São Paulo, inclusive horário de verão:
  anúncios são os novos vistos nas páginas consultadas, mesmo acima do saldo; oportunidades são novas entradas
  na lista. IDs sem observação há 3 dias são esquecidos e podem ser contados de novo se reaparecerem. Os registros
  começam na instalação desta versão; não representam todo o mercado nem alteram a agenda.
- Correção automática: com 5 vendas ou mais de um tipo (desconto ou raro), a mediana de vendido ÷ médio previsto
  das 30 vendas mais recentes desse tipo multiplica o histórico antes de estimar a venda (painel e caça). Só
  desconta, no máximo 20 %; vender acima do previsto não sobe a previsão. Recalcula ao subir e a cada venda informada.

## Compra

- Clique em "Comprar" vira "Confirmar R$ X" por 4 s (evita clique acidental); segundo clique compra.
- Pedido pendente do mesmo anúncio é recuperado do banco após reinício, sem repetir o POST. Resultado
  incerto continua exigindo conferência na Steam; o painel não reconcilia automaticamente essa pendência.
  Confirmação sem código permanece incerta, e uma falha ao gravar a confirmação não interrompe o acompanhamento.
- `buylisting` com os campos de `pesquisa-web.md` 3.1. Se vier `need_confirmation`, o estado vira
  "confirme no app Steam" e o servidor reenvia com o id a cada 2 s por até 60 s.
- Uma compra por vez. Recusa se o total passar do saldo. Saldo atualiza pelo `wallet_info` da resposta.

## Painel (uma seção por vez, pensado para PC 1920×1080, funciona no celular)

- Barra de cima: abas (Oportunidades com a contagem, Radar, Compras só quando há compra, Aprendizado),
  estado da caça ("Caçando 42 skins · 20 consultas totais/min · há 3 s"), saldo editável e botão de
  configurações. A aba fica no endereço (`#radar`), então voltar e recarregar mantêm a seção. A contagem
  de oportunidades também aparece no título da aba do navegador.
- Oportunidades: faixa de uma linha "Melhor combinação" (destaca as skins na lista); seletor "Se eu vender
  pelo preço Rápido / Médio / Alto", que recalcula lucro, margem, preço de venda e prazo e fica salvo no
  navegador; tabela ao vivo com skin, compra, lucro + margem, por dia, vende por (com a faixa do rápido ao
  alto), prazo, risco (selo + confiança), apareceu (+ última conferência) e "Abrir na Steam". "Por dia" é o
  lucro esperado com risco ÷ prazo total e não muda com o seletor. Ordem padrão por lucro por dia; ordenar
  pelo cabeçalho. Linha nova entra com destaque breve; linha vendida vai para "Perdidas nas últimas 24 h".
- Linha expandida (clique em qualquer ponto da linha): gráfico de 30 dias com as três linhas de preço, frase
  de evidência, tabela dos três cenários e "inspecionar no jogo". Explicações longas ficam na dica do
  cabeçalho (passar o mouse), não em parágrafos na tela.
- Radar: skins vigiadas da mais perto de dar lucro para a mais longe, com busca por nome, barra de
  proximidade em "Falta cair", 25 linhas por vez e nome com link para o mercado.
- Compras: o que foi comprado, quando destrava e os três preços para vender.
- Aprendizado: resumo do dia, quatro números do período (7/30/60 dias), gráfico com leitura ao passar o
  mouse e, recolhido, "Diagnóstico da busca" com as tabelas de detalhe.
- Configurações (painel lateral): cada chave com colar/salvar/apagar, estado ok/recusada, endereço do
  painel para os links do Telegram, som liga/desliga, lucro mínimo para avisar.
- Notificação do navegador + som quando chega oportunidade que passa no critério de aviso.

## Como checar

`AGENTS.md` lista os comandos. Todo parser tem fixture em `testdata/` e teste sem rede. O teste final é
usar o painel de verdade no navegador contra a Steam real, com o robô antigo pausado durante a medição.

## Desvios registrados (caça, implementação)

- Braço de float (`f:<nome>` e `h:<nome>`, modo `float`): até 24 skins comuns × desgaste com faixa de float conhecida
  e piso que cabe no saldo; consulta com o desgaste e o filtro de float da faixa de elite (propriedade 2). Faixa
  vista acima do saldo **fica** (até o #102 saía por 6 h): o float bom é anunciado a 2,4–3,8× o preço comum
  (medido 11/11: Serpent Strike FN a R$ 40,33 com o comum a R$ 10,66) e o anúncio mal precificado aparece entre
  os dois. Desde 11/11, nome com faixa aprendida entra se tem
  ≥ 5 vendas/dia e se comprar no piso e vender pelo prêmio medido (limitado a 2×) paga o lucro mínimo, em ordem de
  lucro × vendas/dia × parte das vendas que cai na faixa; nome sem medida entra depois, com ≥ 20 vendas/dia e
  piso ≥ o mínimo do desconto. Antes a ordem era só vendas/dia e 6 dos 16 alvos custavam R$ 0,34 a R$ 1,12, onde
  o prêmio não paga a taxa. Um braço de float anterior (removido no #32) consultava a faixa
  mas avaliava como comum, sem prêmio, então nunca podia achar lucro de float.
  `h:` pesquisa separadamente a AWP Asiimov Battle-Scarred com float ≥ 0,96, faixa visual aproximada da
  Blackiimov; tem preferência dentro das mesmas 16 vagas e exige a mesma prova de vendas para prêmio.
  Patterns de `assets/patterns/named.json` (antes `researched.json`) têm grupos individuais por seed, compatíveis com as chaves
  já aprendidas. Os preços de terceiros selecionam o que pesquisar; nunca viram multiplicador automático.

- Varredura limitada a 6 páginas do topo (60 itens) e 6 amostras da cauda por categoria por ciclo, intercaladas
  entre as categorias: cada página é uma consulta e o catálogo tem milhares. A cauda pega uma página por faixa de
  popularidade que dobra (60–120 … 1920–3840), outra a cada dia: é onde ficam as de giro lento. A varredura
  complementa os metadados e referências já disponíveis no catálogo curado; não define sozinha a cobertura.
- A razão BRL/USD é a mediana de até 3 amostras (menor anúncio em BRL ÷ menor ask em USD do mesmo item, de US$ 1 a
  US$ 20), guardada em `settings` (`usd_brl_ratio`) e remedida a cada 24 h.
- Consulta de desconto leva o filtro de desgaste do nome (`Exterior`); a rara não leva: o código cobre todos os
  desgastes e qualidades, então todos eles (StatTrak/Souvenir inclusive) aparecem na consulta rara e são avaliados
  pelo próprio nome.
- Skins com tabela (cspatterns, blue gem, fade de arma) ficam longe do topo de popularidade (o Moonrise está na
  posição ~1900 das pistolas), então a descoberta faz uma busca por nome de cada uma (`table_skins`, 1 consulta
  por skin a cada ciclo de 24 h) e grava todos os desgastes no catálogo. Faca e fase Doppler não entram (a busca
  descarta ★): a fase é reconhecida na avaliação (`group_of` por paint index) quando o item cai numa tarefa de
  desconto.
- Fila: o vencimento seguinte parte do vencimento anterior (`max(vencimento, agora) + intervalo`), para os pesos
  valerem mesmo com a fila atrasada.
- `/api/watch`: teto de compra ≤ 0 vira nulo e linhas sem teto vão para o fim. Raro mostra teto só quando o
  CSFloat já em cache tem ≥ 3 vendas do grupo (sem rede na rota); senão nulo.
- Pedidos de compra (decisão do Erick: só sugerir, ele abre na Steam): `hunt/orders.rs` gasta uma volta do laço a
  cada 60 s com o livro do alvo comum mais velho (livro com < 30 min não é relido; leitura que falhou conta como lida
  e apaga o livro; raro nunca). `/api/watch` sugere `bid_cents` = maior pedido + 1 centavo quando o livro tem ≤ 1 h,
  cabe no saldo, fica abaixo do menor anúncio (o visto na caça e o do próprio livro, que também limita a revenda e o
  "compraria até"), entra na lista
  (lucro ≥ R$ 1,00 e margem ≥ 10 % no preço médio) e o lucro com o risco dos 7 dias é positivo. Os livros ficam só em
  memória: reiniciar começa do zero.
- Concorrência do `quick` e do `mid`: só conta outro anúncio do mesmo nome e do mesmo grupo raro (ou ambos comuns); um
  comum barato não limita a venda de um T1.
- Anúncio acima do saldo não entra em `listings_seen`: se o saldo subir, ainda é novo. Tarefa com oportunidade
  ativa é preservada para acompanhamento mesmo quando perde o lugar na seleção normal; sair do teste não prova sumiço.
- `gone` só vale com a página confiável: página vazia precisa se repetir 2 vezes, e anúncio fora da primeira
  página (`more`) só some se o preço dele estava dentro da faixa vista.
  A linha encerrada mantém o motivo por 24 h: vendido ou retirado (a Steam não distingue), deixou de compensar,
  sem acompanhamento, comprado pelo usuário ou compra recusada. Uma pausa de consultas não prova venda.
  Ativos são reconferidos após 60 s na faixa da seed encontrada; ler outra faixa não confirma esse anúncio.
  Tentativa sem reencontro ou com erro também espera 60 s antes de repetir, para não prender a agenda.
  O painel mostra quando o anúncio foi conferido e indica espera após 3 min sem confirmação; isso não informa
  a idade de criação na Steam. O registro só tem limite de idade quando uma consulta anterior cobria seu preço.
- Reavaliação (revisão do Codex): anúncio já visto é avaliado de novo quando o preço, o histórico do item ou o menor concorrente mudam
  (e, se é oportunidade ativa com histórico velho, a cada passagem). A oportunidade atualizada mantém a posição
  (`seen_at`) e só republica o evento se algo mudou; se deixou de compensar, vira `gone`. O aviso do Telegram sai
  uma vez após qualificar e confirmar a entrega, mesmo que isso só ocorra na reavaliação. O mapa de avaliados
  vive em memória (reavalia tudo uma vez após reiniciar).
- Balanço de 10 min: `State.hunt.message` diz quantos anúncios foram conferidos e o quanto o mais perto dava de
  lucro (ou prejuízo); o log INFO traz os 3 mais perto, com o lucro no preço médio e no rápido.
  A mesma linha de 10 min conta as respostas da Steam por saída (`principal_ok`, `principal_429`, `extra_ok`,
  `extra_429`): é o que mede se os bloqueios longos vêm da quantidade de consultas ou do tempo seguido caçando.
- `QueryListingsForItem` com 200 sem `data` conta como falha do endpoint (breaker) e loga status, tamanho, nomes
  das chaves do topo e `success`; nunca o conteúdo.
- Compra: uma por vez, do pedido até confirmar/terminar e ajustar o saldo; a segunda recebe 409 "termine a compra
  em andamento primeiro".

### Ensaio offline da agenda marginal em 16/11/2026

2 dias × 3 sorteios por regra, release, SALDO=3608 e RATIO=5.5 hipotéticos; sem rede.
Top5 exclui controle; soma as cinco faixas mais frequentes de cada sorteio. Maior gap inclui todas as leituras e bordas do horizonte.

| Mercado/parâmetros | Vida (min) | Leituras por regra | Capturas/chegadas antiga → nova | Valor simulado (centavos) antiga → nova | Top5 agenda antiga → nova | Maior gap antiga → nova |
|---|---:|---:|---|---|---|---|
| mercado observado, parâmetros default | 15 | 18516 | 33/125 → 39/125 | 4479.91 → 4431.75 | 12211/16203 (75.36%) → 5928/16203 (36.59%) | 784.0 → 586.1 min |
| mercado observado, parâmetros default | 120 | 18516 | 67/125 → 84/125 | 6104.12 → 6460.17 | 11642/16203 (71.85%) → 4919/16203 (30.36%) | 742.9 → 601.1 min |
| mercado observado, parâmetros default | 720 | 18516 | 100/125 → 119/125 | 6831.32 → 7045.39 | 10142/16203 (62.59%) → 4047/16203 (24.98%) | 724.3 → 526.4 min |
| mercado observado, parâmetros janela das faixas | 15 | 18516 | 37/125 → 42/125 | 4290.65 → 3707.64 | 11172/16203 (68.95%) → 4742/16203 (29.27%) | 724.3 → 462.9 min |
| mercado observado, parâmetros janela das faixas | 120 | 18516 | 74/125 → 92/125 | 6231.91 → 6787.51 | 10225/16203 (63.11%) → 4191/16203 (25.87%) | 705.6 → 507.7 min |
| mercado observado, parâmetros janela das faixas | 720 | 18516 | 111/125 → 120/125 | 6988.79 → 7069.84 | 9123/16203 (56.30%) → 3440/16203 (21.23%) | 642.1 → 414.4 min |
| mercado giro, parâmetros default | 15 | 18516 | 12/51 → 16/51 | 1488.71 → 1537.82 | 11686/16203 (72.12%) → 5538/16203 (34.18%) | 750.4 → 522.7 min |
| mercado giro, parâmetros default | 120 | 18516 | 21/51 → 30/51 | 3246.64 → 3425.33 | 11171/16203 (68.94%) → 4279/16203 (26.41%) | 750.4 → 567.5 min |
| mercado giro, parâmetros default | 720 | 18516 | 39/51 → 42/51 | 3494.41 → 3557.39 | 9213/16203 (56.86%) → 3575/16203 (22.06%) | 694.4 → 481.1 min |
| mercado giro, parâmetros janela das faixas | 15 | 18516 | 12/51 → 12/51 | 1332.81 → 2567.73 | 10538/16203 (65.04%) → 4829/16203 (29.80%) | 698.1 → 421.4 min |
| mercado giro, parâmetros janela das faixas | 120 | 18516 | 23/51 → 33/51 | 3253.83 → 3437.40 | 9762/16203 (60.25%) → 3126/16203 (19.29%) | 698.1 → 421.4 min |
| mercado giro, parâmetros janela das faixas | 720 | 18516 | 41/51 → 45/51 | 3525.50 → 3561.68 | 8278/16203 (51.09%) → 2889/16203 (17.83%) | 664.5 → 421.4 min |

Capturas não pioraram; valor simulado caiu até 13,59% em alguns cenários.
O modelo começa frio, não reproduz necessariamente a concentração real e não prova melhora de lucro real.
Cobertura de 227 em todas as linhas é garantida pelo controle neste ensaio e não prova espalhamento.
