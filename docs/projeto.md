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

Não é objetivo da v1: vender pelo painel, valorizar adesivo/chaveiro, auto-compra, facas e luvas. Auto-compra e confirmação sem celular: plano futuro em
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
| `hunt/scheduler.rs` | fila de consultas por prioridade dentro do ritmo do limitador |
| `hunt/detect.rs` | anúncio novo → avaliação → oportunidade / some quando vendido |
| `hunt/ledger.rs` | registro de observações: todo anúncio visto, idade, sumiço e relistagem (aprendizado) |
| `hunt/teacher.rs` | professor grátis: compras virtuais dos avisos e quase-achados e o resultado delas no mercado; recalibração do comum |
| `hunt/value.rs` | valor de uma leitura (taxa de anúncio novo × estoque acumulado × retorno) e o ajuste horário dos parâmetros |
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
GET  /api/purchases        -> Purchase[]
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
             kind: "desconto" | "raro" | null, sold_cents: number | null }
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
2. **Seleção:** vigia itens com piso entre o mínimo viável e o saldo e ≥ 1 venda/dia. Nota = vendas/dia
   (até 10: quem compra uma unidade não ganha com giro maior) × lucro possível em reais × fator aprendido do alvo (abaixo). Lucro possível = `seller_gets(piso ÷ (1 − dispersão))` − piso,
   com dispersão (P50−P10)/P50 limitada a 50 %: o preço da taxa já entra, então item barato ou de pouca folga fica
   no fim da fila (não sai: um anúncio fora da curva ainda pode dar lucro). Mais **uma** consulta de grupo raro
   por skin com tabela (código G, **sem filtro de desgaste**: a mesma consulta traz todos os desgastes, StatTrak e
   Souvenir da faixa) cujo piso, no desgaste mais barato, cabe no saldo (`propertyFilters` por faixa de seed dos
   tiers; a tarefa percorre as faixas em rodízio, uma por consulta). Até 24 raros, os que mais vendem por dia
   primeiro (soma dos desgastes; sem venda medida conta 1/dia): giro alto traz anúncio novo, e a revenda sai mais
   rápido. Antes era uma tarefa por desgaste: o Moonrise ocupava 5 vagas e cada faixa era revista 5× menos.
   - **Piso:** o do anúncio visto na última consulta da tarefa (vale 6 h); sem consulta ainda, USD × razão medida,
     que só serve de triagem (a Steam em BRL não segue a razão por item; o raro T1 chega a custar dezenas de vezes
     o piso comum da skin). Desconto com anúncio visto acima do saldo sai; raro sai quando **todas** as faixas
     foram vistas, todas tinham anúncio e o menor passa do saldo (faixa vazia mantém: é onde o anúncio aparece);
     enquanto faltam faixas, o raro com piso visto acima do saldo entra com 1/4 da nota-base.
     A seleção é refeita a cada 15 min e no próximo passo após observar um piso novo acima do saldo.
   - **Agenda que aprende:** cada alvo (skin × desgaste × modo: `barato`, `seed` ou `float`) recebe a leitura
     quando o **valor dela agora**, em centavos, é o maior: taxa de anúncio novo do alvo × anúncios acumulados desde
     a última leitura × retorno por anúncio novo (fórmula, ajuste e medidas no item 6 do Aprendizado). Sinal =
     anúncio **novo** que cabe no saldo (na leitura só pelo livro, a queda do menor anúncio); reavaliar o mesmo
     anúncio não conta de novo. Alvo nunca lido vai na
     frente; empate vai para o lido há mais tempo. Regras que não mudam: 12,5 % das consultas vão sempre ao alvo
     menos testado, com o slot girando entre os modos (cada modo tem piso próprio, qualquer que seja o número de
     alvos). O saldo e a taxa continuam decidindo quem entra (regras acima); a nota-base do catálogo só desempata
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
3. **Ritmo:** grupos de controle independentes: actions de anúncios/histórico; livro/busca/interface antiga;
   sessão/login/compra. São divisões operacionais baseadas nas respostas medidas, não cotas oficiais da Valve.
   Sessão e compra não herdam o 429 de uma varredura de anúncios. Entre duas consultas quaisquer há no mínimo 3 s, de
   qualquer balde ou saída: nunca rajada. As leituras de mercado somadas (todos os endpoints) têm ainda um piso de 12 s (300/h), inclusive
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
   extra leva toda leitura pública (anúncios, histórico, livro, busca, interface antiga) e tem o próprio piso de
   12 s entre leituras; o mínimo de 3 s entre quaisquer envios continua somando as duas. Desenho anterior: um proxy HTTP em `127.0.0.1:1056` (`ALT_PROXY`; no PC do Erick é o `saida-celular`, que sai pela
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
   escolhido 24 s depois da última leitura de anúncios (≤ 150/h); a vaga do meio vai a um alvo barato, pelo livro.
   Em 10/11, ~200 leituras de anúncios/h por 3 h terminaram em 429 desse balde por horas, com o livro respondendo;
   124–153/h tinham rodado 3–4 h sem 429. O número é hipótese de poucas horas. O achado ativo é conferido a cada
   60 s enquanto é novo e depois a cada décimo da idade, até 30 min: um achado parado há 5 h gastava 40 leituras
   de anúncios por hora. Venda do CSFloat com chaveiro não é venda limpa (como a com adesivo): o chaveiro paga
   acima da base e não prova pattern. **Bloqueio sem cegueira (10/11):** com a Steam barrando o histórico de
   preços, a avaliação usa o guardado de até 24 h e escreve a idade na evidência; antes, todo candidato de nome com
   histórico vencido (6 h) ficava "adiado" enquanto o bloqueio durasse. Nome sem histórico guardado continua sem
   avaliação. 24 h é hipótese. No painel, achado que passou do prazo de conferência (o de `recheck_secs`, mais
   2 min) aparece apagado, com "Sem conferir há …", e não conta no número da aba: em 10/11 um achado de seed raro
   ficou 3 h na lista de abertos sem que o app pudesse olhá-lo. O teto desses dois filtros não conta o menor anúncio do livro como
   concorrente: ele é o próprio candidato, e derrubaria o teto abaixo de si mesmo. Consultas de anúncios idênticas compartilham a página. O log de ciclo conta consultas de anúncios economizadas. Cada consulta devolve os anúncios mais baratos; `listingid` novo com preço ≤ saldo vai para
   avaliação; anúncio que sumiu vira evento `gone`.

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
     sumiram em até 48 h sem relistagem (múltiplo = preço ÷ preço médio comum da skin, sem prêmio).
   - Prêmio = média geométrica encolhida seed → grupo → 1× com 5 vendas fictícias por nível; p25/p75 das vendas da
     seed quando ela tem ≥ 10, senão das do grupo.
   - "Comprovado" = ≥ 10 vendas fechadas e vender a `base × p25` ainda dá o lucro e a margem mínimos do aviso
     (R$ 1,00 e 10 %). Senão, "ainda não comprovado" com o motivo (quantas vendas, ou quanto dá o pior quartil).
   - Preço sugerido: anunciar a `base × p75` e descer até `base × prêmio`. Anúncios do grupo à venda há mais de
     48 h (vistos nas últimas 6 h) aparecem como "parados".
   - Sai no painel (etiqueta ao lado do grupo e linha nos detalhes), no aviso do Windows e no Telegram.
6. **Valor de uma leitura (`hunt/value.rs`).** Projeto e medidas:
   `docs/superpowers/specs/2026-11-10-aprendizado-unificado-design.md`. Substitui o crédito por proximidade do
   lucro (≈ 0 em 99,9 % dos anúncios) e a colheita em sombra.
   - `V = taxa × L·(1 − e^(−Δ/L)) × retorno`. `Δ` = segundos desde a última vez que a agenda olhou o alvo (leitura
     vazia ou com falha também conta); `L` = percentil 25 da vida (7 dias) dos anúncios que eram o mais barato e
     sumiram sem relistagem, com ≥ 20 medidas (piso 60 s); senão 900 s.
   - **Taxa** (anúncios novos por segundo): `a = r + m·W·λ`, `taxa = (a + √a) ÷ (exposição + m·W)`. `r` = anúncios
     novos do alvo; exposição = Σ `W·(1 − e^(−intervalo/W))` das leituras (a primeira conta `W`); as duas pesam
     metade a cada `H`. `λ` = Σr ÷ Σexposição da classe (`d`/`r`/`f`): o alvo novo começa na média dos irmãos. `√a`
     é o otimismo de contagem, que some com medida.
   - **Teto por seca:** sem nenhum anúncio novo em `s` segundos de exposição seguidos, a taxa não passa de `3 ÷ s`
     (regra de três). O alvo que secou perde espaço em horas, não em dias; o primeiro anúncio novo zera a seca.
   - **Retorno** (centavos por anúncio novo, vida inteira): `(N0·w_c + ganho) ÷ (N0 + novos)`, com `w_c` da classe
     encolhido para o total. Sem nenhum ganho medido, retorno = 1 para todos (só a taxa decide).
   - **Ajuste de hora em hora, só banco:** repete as leituras dos últimos 7 dias (`reads`; páginas do mesmo alvo
     em até 90 s contam como uma) para `W` {0,5; 1; 2; 4; 8 h} × `m` {0,5; 1; 2; 4} × `H` {24; 72; 168 h} e mede o
     erro de previsão (perda de Poisson, cada leitura prevista só com o passado). Troca os parâmetros só com
     ≥ 1 000 leituras pontuadas e melhora > 1 %; a troca vai para `refit_log` (`fluxo:janela`, `fluxo:prior`,
     `fluxo:meia_vida`) e refaz `r`/exposição dos alvos. Resumo em `settings.value_info`, mostrado no placar:
     erro do modelo × erro de uma taxa fixa por classe, previsto × real, anúncios novos por leitura.
   - Padrões `W` = 2 h, `m` = 1, `H` = 72 h: medidos no histórico de 10/11 (erro 0,32 contra 0,55 da taxa por
     classe). `N0` = 500, o otimismo, o p25 da vida, a margem de 1 % e o teto de 3 são hipóteses de calibração.
   - **Prova contra no prêmio aprendido (`patterns::load`):** o seed já provado (≥ 3 vendas caras) leva também as
     vendas dele abaixo de 2× a mediana da skin (`csfloat_sales`) nos quantis do prêmio. Venda comum não completa
     prova.
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
- Float de elite: anúncio comum cujo float está nos 10 % melhores do desgaste (a partir do menor float da skin) é
  raro do grupo `float:<skin>:<desgaste>` (pattern vence float). O prêmio vem das vendas do CSFloat da mesma faixa,
  pela mesma regra do raro abaixo; sem 3 vendas, não há prêmio. Medido em 06/11 (157 skins, ~6 000 vendas do
  CSFloat): os 10 % melhores saíram de 1,3× a 2,8× a mediana em várias skins (XM1014 Zombie Offensive FN 2,8×, USP-S
  Tropical Breeze FN 2,3×, M4A1-S Solitude MW 1,7×); no Moonrise FT as duas vendas da faixa saíram a ~1,0×. Os 10 %
  são recorte de hipótese, não calibração.
- Pattern aprendido (`src/hunt/patterns.rs`): as tabelas só cobrem 4 skins de pattern, e nas vendas do CSFloat de
  06/11 apareceram seeds caros fora delas (MP7 Amberline 890 a 5–10×, M4A4 Etch Lord 158 a 18×). Um coletor lê as
  vendas do CSFloat de uma skin do catálogo a cada 2 min (a de mais vendas/dia com piso ≤ saldo, ≥ 5 vendas/dia, cada
  skin no máximo 1×/24 h; não gasta consulta da Steam). A mesma vaga prioriza nomes vigiados da política normal
  cujo cache de vendas falta ou venceu (6 h). O aquecimento desses nomes usa intervalo de 15 s enquanto a
  última consulta não falhou; fora dele, mantém 2 min. A cota e a pausa do CSFloat continuam obrigatórias;
  falhas por nome esperam 10 min. O cache completo persiste no banco com a hora real da busca e só volta após
  reinício enquanto tiver menos de 6 h; dados corrompidos ou com data futura são ignorados. Esses prazos são
  hipóteses e a candidata externa continua fora dessa prioridade. Com ≥ 10 vendas limpas, guarda em
  `learned_sales` cada venda sem adesivo, fora de tabela e float de elite, que saiu entre 2× e o teto do múltiplo
  raro sobre a mediana da skin.
  Com 3 vendas assim do mesmo seed (somando desgastes e StatTrak, últimos 60 dias) o seed vira grupo
  `learned:<skin>:<seed>` ("Pattern #seed"), o prêmio são as razões guardadas, e entra nas faixas de seed da busca
  rara da skin junto com as da tabela. Os limites (2×, 3 vendas, 60 dias) são hipóteses, não calibração; demora
  dias para juntar prova.
- Raro: o grupo do anúncio (tier, fase, faixa de fade, float de elite) busca vendas do CSFloat do mesmo grupo
  (`price / reference.base_price`). Com n ≥ 3: `mid` e `high` da base × Q50/Q75 do múltiplo; `quick` fica sem
  prêmio (× min(1, Q25)), porque o prêmio do CSFloat já saiu abaixo na Steam (`conhecimento.md` 5.6). Com n < 3:
  usa a base comum e escreve na evidência "prêmio sem prova (n vendas)". Preço pedido nunca vira preço.
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

- Braço de float (`f:<nome>` e `h:<nome>`, modo `float`): até 16 skins comuns × desgaste com faixa de float conhecida, ≥ 20 vendas/dia
  e piso que cabe no saldo, as que mais vendem primeiro; consulta com o desgaste e o filtro de float da faixa de elite
  (propriedade 2). Faixa vista acima do saldo sai até a consulta envelhecer (6 h). Não usa o piso mínimo de desconto:
  o prêmio de float paga a taxa mesmo em skin barata. Um braço de float anterior (removido no #32) consultava a faixa
  mas avaliava como comum, sem prêmio, então nunca podia achar lucro de float.
  `h:` pesquisa separadamente a AWP Asiimov Battle-Scarred com float ≥ 0,96, faixa visual aproximada da
  Blackiimov; tem preferência dentro das mesmas 16 vagas e exige a mesma prova de vendas para prêmio.
  Patterns de `assets/patterns/researched.json` têm grupos individuais por seed, compatíveis com as chaves
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
