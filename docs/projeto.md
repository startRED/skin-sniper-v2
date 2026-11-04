# Skin Sniper v2 — desenho

Fonte de fatos da Steam: `docs/conhecimento.md` (o que o projeto antigo mediu) e `docs/pesquisa-web.md`
(sondas e fontes de 03/11/2026). Este arquivo diz **o que** construir; aqueles dizem **como a Steam responde**.
Quando um fato mudar, corrija lá, não aqui.

## Objetivo

O usuário abre o painel, vê o saldo (lido da Steam ou digitado) e uma lista viva de anúncios que dão lucro
real depois das taxas, que cabem no saldo. Ele ordena por lucro ou margem e compra com um clique. O Skin
Sniper escolhe sozinho o que caçar. Leve no PC: um binário Rust, um banco SQLite que se limpa sozinho.

Não é objetivo da v1: vender pelo painel, valorizar adesivo/chaveiro, auto-compra, facas e luvas.

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
| `steam/client.rs` | HTTP único da Steam: cookie da sessão, limitador adaptativo, breakers por endpoint |
| `steam/listings.rs` | código G, `QueryListingsForItem` (com `propertyFilters`), parse de `asset_properties` |
| `steam/history.rs` | `QueryPriceHistory` → pontos `(ts, mediana_centavos, compras)` |
| `steam/search.rs` | `search/render` para descoberta de catálogo (USD → BRL por razão medida) |
| `steam/session.rs` | valida o `steamLoginSecure`, obtém `sessionid`, lê saldo da carteira |
| `steam/buy.rs` | `buylisting` com o fluxo `need_confirmation` |
| `csfloat.rs` | `/history/<nome>/sales` → múltiplos `price / reference.base_price` por venda |
| `pricing/fees.rs` | taxa em centavos inteiros e inverso por busca binária |
| `pricing/sell.rs` | três preços de venda, prazo, confiança |
| `pricing/groups.rs` | grupo raro de um anúncio (tier de pattern, fase Doppler, faixa de fade) a partir das tabelas |
| `pricing/combo.rs` | melhor combinação de anúncios que cabe no saldo (mochila em centavos) |
| `hunt/catalog.rs` | descoberta e seleção do que vigiar dado o saldo |
| `hunt/scheduler.rs` | fila de consultas por prioridade dentro do ritmo do limitador |
| `hunt/detect.rs` | anúncio novo → avaliação → oportunidade / some quando vendido |
| `notify.rs` | Telegram + evento SSE para notificação/som no navegador |
| `web/` | rotas, SSE, guarda de Host, arquivos embutidos |

Tabelas de pattern copiadas do projeto antigo para `assets/patterns/` (ver `conhecimento.md` 4.5).

## Contrato da API (o frontend depende só disto)

Valores de dinheiro sempre em **centavos BRL inteiros**. Datas em segundos Unix.

```
GET  /api/state            -> State
PUT  /api/saldo            {cents: int}            -> State      (saldo manual; null volta a ler da Steam)
GET  /api/opportunities    -> Opportunity[]         (ativas e as perdidas do último dia, até 50, com gone_at; o frontend ordena)
GET  /api/opportunities/:id/history -> {points: [[ts, median_cents, purchases]], sell: Sell}
POST /api/buy              {id: string}            -> Purchase
GET  /api/purchases        -> Purchase[]
POST /api/purchases/:id/sold {sold_cents: int}      -> Purchase (valor pago pelo comprador, com taxas)
GET  /api/calibration      -> Calibration
GET  /api/watch            -> WatchRow[]            (o que a caça vigia; mais perto de compensar primeiro)
GET  /api/settings         -> Settings              (chaves mascaradas)
PUT  /api/settings         Partial<SettingsInput>   -> Settings   (campo null apaga)
GET  /api/events           SSE: "opportunity" Opportunity | "gone" {id} | "state" State | "purchase" Purchase
```

```ts
State = {
  saldo_cents: number, saldo_source: "steam" | "manual" | "none",
  hunt: { watching: number, queries_per_min: number, last_query_at: number | null,
          paused_until: number | null, message: string | null },
  combo: { ids: string[], cost_cents: number, profit_cents: number },   // melhor combinação no saldo
  keys: { steam: KeyStatus, csfloat: KeyStatus, telegram: KeyStatus }
}
KeyStatus = { set: boolean, ok: boolean | null, detail: string | null }  // detail: conta, erro...
Opportunity = {
  id: string,                    // listingid
  name: string,                  // market_hash_name
  icon_url: string, steam_url: string, inspect_url: string | null,
  wear: string | null, float: number | null, seed: number | null,
  group: string | null,          // ex.: "T1 Center Star", "Ruby", "Fade 99%"
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
  seen_at: number
}
WatchRow = {
  name: string, icon_url: string,
  floor_cents: number | null,      // menor anúncio visto na última consulta; null se ainda não consultou / página vazia
  max_buy_cents: number | null,    // maior preço que ainda dá lucro >= R$ 0,20 e >= 5 % no preço médio;
                                   // null sem estimativa e sempre null em item raro (o teto depende do grupo)
  sales_per_day: number, kind: "desconto" | "raro", group: string | null,
  last_checked_at: number | null,  // ordem: (floor - max_buy) crescente; sem os dois, por último
  mode: "barato" | "seed", // modo de busca do alvo (a mesma skin pode aparecer em mais de um modo)
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
Settings = { steam_login_secure: string|null, csfloat_key: string|null, telegram_token: string|null,
             telegram_chat_id: string|null, panel_url: string|null, sound: boolean,
             notify_min_profit_cents: number }      // segredos chegam mascarados: "••••gLO9"
```

## Acesso ao painel

- Sem login nem token (decisão do Erick). Escuta em todas as interfaces.
- Duas proteções invisíveis contra outro site disparar compra pelo navegador do Erick:
  1. Host aceito só se for IP literal, `localhost`, `*.ts.net` ou o nome do PC (anti DNS rebinding).
  2. Requisição que muda algo exige `Content-Type: application/json`; o servidor não manda cabeçalhos CORS
     (força preflight que falha para outra origem).

## Caça

1. **Descoberta (1×/dia e quando o saldo muda muito):** `search/render` ordenado por popularidade nas
   categorias de arma, sem faca/luva, até cobrir os itens com piso ≤ saldo. Para cada candidato,
   `QueryPriceHistory` (cache 6 h) dá vendas/dia e dispersão.
2. **Seleção:** vigia itens com piso entre o mínimo viável e o saldo e ≥ 1 venda/dia. Nota = vendas/dia
   (até 10: quem compra uma unidade não ganha com giro maior) × lucro possível em reais × (1 + acertos recentes). Lucro possível = `seller_gets(piso ÷ (1 − dispersão))` − piso,
   com dispersão (P50−P10)/P50 limitada a 50 %: o preço da taxa já entra, então item barato ou de pouca folga fica
   no fim da fila (não sai: um anúncio fora da curva ainda pode dar lucro). Mais **uma** consulta de grupo raro
   por (skin com tabela × desgaste) cujo piso cabe no saldo (`propertyFilters` por faixa de seed dos tiers; a
   tarefa percorre as faixas em rodízio, uma por consulta).
   - **Piso:** o do anúncio visto na última consulta da tarefa (vale 6 h); sem consulta ainda, USD × razão medida,
     que só serve de triagem (a Steam em BRL não segue a razão por item; o raro T1 chega a custar dezenas de vezes
     o piso comum da skin). Desconto com anúncio visto acima do saldo sai; raro sai quando **todas** as faixas
     foram vistas, todas tinham anúncio e o menor passa do saldo (faixa vazia mantém: é onde o anúncio aparece);
     enquanto faltam faixas, o raro com piso visto acima do saldo entra com 1/4 da nota-base.
     A seleção é refeita a cada 15 min para isso valer.
   - **Agenda que aprende (bandido de vários braços):** cada alvo (skin × desgaste × modo: `barato` ou `seed`) é um braço com uma nota que se atualiza sozinha = lucro esperado (preço médio) achado por consulta
     gasta; quase-lucro (anúncio que faltou até R$ 0,30 ou 10 % do preço para entrar na lista) conta 15 % do
     lucro mínimo, para a nota não começar do zero. A agenda é **UCB1** (determinístico, sem crate de aleatórios):
     quem rende por consulta ganha mais consultas, e a frequência de cada alvo é essa alocação. Regras que não
     mudam: 12,5 % das consultas vão sempre ao braço menos testado, com o slot girando entre os dois modos (cada
     modo tem piso próprio, qualquer que seja o número de braços); consultas e recompensa pesam metade a cada
     24 h; a nota vale metade a cada 4 h sem nenhuma recompensa. O saldo e a taxa continuam decidindo quem
     entra (regras acima); a nota-base do catálogo só desempata braços ainda não testados. A busca e as consultas de anúncios dividem o mesmo limitador global. As notas ficam no banco (`arm_stats`, retenção 14 dias
     e no máximo 2000 linhas) e sobrevivem a reinício. Os parâmetros (12,5 %, meias-vidas, 15 %) são hipóteses de calibração, não medidas.
   - **Piso mínimo de desconto:** menor preço em que uma venda 30 % acima ainda deixa R$ 0,20 e 5 % depois da taxa
     (hoje ≈ R$ 1,45). Abaixo disso a taxa mínima de 1 + 1 centavo come o lucro. Os 30 % são hipótese, não medida.
3. **Ritmo:** um limitador global. Começa com 1 consulta a cada 6 s (10/min; com 3 s e 4 s houve 429 por horas, e o robô antigo leu a ~6–12/min sem 429), que também é o piso. A leitura do mercado vai logada quando há cookie; um 429 numa leitura logada (anúncios ou histórico) não pausa: as leituras seguem sem login por 6 h. Um 429
   pausa 60 s (dobrando até 30 min: com tentativas a cada 10 min a penalidade de 04/11 não esfriava) e alonga o intervalo 50 %; cada 10 min sem 429 encurta 15 % (de 60 s a 6 s em ~2,5 h), até voltar a 6 s. O degrau da pausa só volta a 60 s após 60 min sem 429.
   Pausa e intervalo do último 429 ficam salvos (`steam_pace`) e valem depois de reiniciar; o tempo quieto desde o último encurtamento salvo continua encurtando. Breaker por endpoint.
   Os números ficam no `State` para o painel mostrar.
4. **Triagem e detecção:** tarefas de desconto da mesma skin-base compartilham uma busca; preço em USD convertido pela razão medida até 10 % acima do teto prioriza anúncios. A tolerância e o frescor do preço da busca são hipóteses, sem medição. Resultado ou teto ausente mantém consulta direta; oportunidades ativas também são conferidas. Consultas de anúncios idênticas compartilham a página. O log de ciclo conta consultas de anúncios economizadas. Cada consulta devolve os anúncios mais baratos; `listingid` novo com preço ≤ saldo vai para
   avaliação; anúncio que sumiu vira evento `gone`.

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
- Raro: o grupo do anúncio (tier, fase, faixa de fade) busca vendas do CSFloat do mesmo grupo
  (`price / reference.base_price`). Com n ≥ 3: `mid` e `high` da base × Q50/Q75 do múltiplo; `quick` fica sem
  prêmio (× min(1, Q25)), porque o prêmio do CSFloat já saiu abaixo na Steam (`conhecimento.md` 5.6). Com n < 3:
  usa a base comum e escreve na evidência "prêmio sem prova (n vendas)". Preço pedido nunca vira preço.
- Prazo `eta_hours` = 24 / (vendas/dia × fração do peso com mediana ≥ preço), depois da trava.
- Confiança: alta (≥ 20 vendas/dia e ≥ 100 pontos em 7 dias; raro com n ≥ 8), média (≥ 5/dia; raro n ≥ 3),
  baixa no resto.
- Nota por dia é uma hipótese, não probabilidade medida de venda: em cada preço, calcula a fração do peso
  dos últimos 7 dias com preço projetado igual ou maior; no raro comprovado, combina cada ponto com cada
  múltiplo válido do grupo. Normaliza as três frações para somarem 1 e faz a média ponderada dos lucros
  líquidos. Desconta `preço de compra × min(1, volatilidade × √7)`: penalidade conservadora para possível
  queda durante a trava, não chance estatística de queda. Arredonda o resultado em centavos.
  Volatilidade é o desvio padrão populacional das variações entre medianas diárias ponderadas por compras,
  em dias consecutivos dos últimos 30 dias; exige pelo menos duas variações. Sem isso, lucro esperado é nulo.
  Dias presos = 7 + prazo médio em horas / 24; sem prazo medido (incluindo raro comprovado), dias e nota são
  nulos. Nota = lucro esperado / dias presos, arredondada em centavos; nulos ficam por último.
- Risco baixo: volatilidade ≤ 2 % ao dia, ≥ 20 vendas/dia e, em raro, n ≥ 8. Médio: volatilidade ≤ 5 %,
  ≥ 5 vendas/dia e, em raro, n ≥ 3. Alto nos demais casos, inclusive sem volatilidade suficiente. Esses
  limites são hipóteses para priorização, não calibração com vendas reais.
- Combinação: usa lucro esperado ajustado ao risco positivo; sem estimativa ou com resultado ≤ 0, o item não entra. Se a de maior lucro tiver uma única skin, prefere duas ou três de nomes distintos quando
  o lucro delas alcançar pelo menos 90 % do ótimo e couber no saldo.
- Entra na lista: lucro no `mid` ≥ R$ 0,20 e margem ≥ 5 % (o risco aparece na linha; a lista não esconde por ele).
  `safe` quando lucro no `quick` > 0. Combinação e aviso exigem lucro esperado ajustado ao risco positivo.
  Aviso no Telegram/som: lucro esperado ajustado ao risco positivo, `safe` e lucro no `mid` ≥ `notify_min_profit_cents` (padrão R$ 1,00).
- Calibração: cada compra grava os três preços previstos; quando o Erick vender, o realizado entra numa
  tabela para conferir o erro (v1 só grava; o ajuste fino vem depois com dados).
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
- `buylisting` com os campos de `pesquisa-web.md` 3.1. Se vier `need_confirmation`, o estado vira
  "confirme no app Steam" e o servidor reenvia com o id a cada 2 s por até 60 s.
- Uma compra por vez. Recusa se o total passar do saldo. Saldo atualiza pelo `wallet_info` da resposta.

## Painel (uma tela, pensado para PC 1920×1080, funciona no celular)

- Barra de cima: saldo (editável, mostra se veio da Steam), estado da caça ("Caçando 42 skins ·
  20 consultas/min · há 3 s"), botão de configurações.
- Faixa "Melhor combinação com R$ 36,08": as skins, custo e lucro provável; clicar destaca na tabela.
- Tabela única, densa, atualizada ao vivo: skin (imagem, nome, desgaste, float, seed, grupo), preço,
  três preços de venda, lucro, margem, prazo, confiança, há quanto tempo apareceu, botão comprar.
  Ordem padrão por lucro esperado por dia; ordenar clicando no cabeçalho (por dia, lucro, margem, lucro seguro, prazo, mais novo). Linha nova entra com
  destaque breve; linha vendida sai.
- Linha expandida: gráfico de 30 dias com as três linhas de preço, frase de evidência, links "abrir na
  Steam" e "inspecionar no jogo".
- Compras: seção abaixo da tabela com o que foi comprado, quando destrava e os três preços para vender.
- Configurações (painel lateral): cada chave com colar/salvar/apagar, estado ok/recusada, endereço do
  painel para os links do Telegram, som liga/desliga, lucro mínimo para avisar.
- Notificação do navegador + som quando chega oportunidade que passa no critério de aviso.

## Como checar

`AGENTS.md` lista os comandos. Todo parser tem fixture em `testdata/` e teste sem rede. O teste final é
usar o painel de verdade no navegador contra a Steam real, com o robô antigo pausado durante a medição.

## Desvios registrados (caça, implementação)

- Varredura limitada a 6 páginas do topo (60 itens) e 6 amostras da cauda por categoria por ciclo, intercaladas
  entre as categorias: cada página é uma consulta e o catálogo tem milhares. A cauda pega uma página por faixa de
  popularidade que dobra (60–120 … 1920–3840), outra a cada dia: é onde ficam as de giro lento. Os candidatos vêm só do que a varredura viu.
- A razão BRL/USD é a mediana de até 3 amostras (menor anúncio em BRL ÷ menor ask em USD do mesmo item, de US$ 1 a
  US$ 20), guardada em `settings` (`usd_brl_ratio`) e remedida a cada 24 h.
- Consulta de desconto e de raro levam o filtro de desgaste do nome (`Exterior`); o código cobre todos os
  desgastes e qualidades, então StatTrak/Souvenir do mesmo código aparecem na consulta rara e são avaliados.
- Skins com tabela (cspatterns, blue gem, fade de arma) ficam longe do topo de popularidade (o Moonrise está na
  posição ~1900 das pistolas), então a descoberta faz uma busca por nome de cada uma (`table_skins`, 1 consulta
  por skin a cada ciclo de 24 h) e grava todos os desgastes no catálogo. Faca e fase Doppler não entram (a busca
  descarta ★): a fase é reconhecida na avaliação (`group_of` por paint index) quando o item cai numa tarefa de
  desconto.
- Fila: o vencimento seguinte parte do vencimento anterior (`max(vencimento, agora) + intervalo`), para os pesos
  valerem mesmo com a fila atrasada.
- `/api/watch`: teto de compra ≤ 0 vira nulo e linhas sem teto vão para o fim. Raro mostra teto só quando o
  CSFloat já em cache tem ≥ 3 vendas do grupo (sem rede na rota); senão nulo.
- Concorrência do `quick` e do `mid`: só conta outro anúncio do mesmo nome e do mesmo grupo raro (ou ambos comuns); um
  comum barato não limita a venda de um T1.
- Anúncio acima do saldo não entra em `listings_seen`: se o saldo subir, ainda é novo. Tarefa que sai da seleção
  encerra (`gone`) as oportunidades dela, porque ninguém mais as confere.
- `gone` só vale com a página confiável: página vazia precisa se repetir 2 vezes, e anúncio fora da primeira
  página (`more`) só some se o preço dele estava dentro da faixa vista.
- Reavaliação (revisão do Codex): anúncio já visto é avaliado de novo quando o preço, o histórico do item ou o menor concorrente mudam
  (e, se é oportunidade ativa com histórico velho, a cada passagem). A oportunidade atualizada mantém a posição
  (`seen_at`) e só republica o evento se algo mudou; se deixou de compensar, vira `gone`. O aviso do Telegram sai
  só para oportunidade nova. O mapa de avaliados vive em memória (reavalia tudo uma vez após reiniciar).
- Balanço de 10 min: `State.hunt.message` diz quantos anúncios foram conferidos e o quanto o mais perto dava de
  lucro (ou prejuízo); o log INFO traz os 3 mais perto, com o lucro no preço médio e no rápido.
- `QueryListingsForItem` com 200 sem `data` conta como falha do endpoint (breaker) e loga status, tamanho, nomes
  das chaves do topo e `success`; nunca o conteúdo.
- Compra: uma por vez, do pedido até confirmar/terminar e ajustar o saldo; a segunda recebe 409 "termine a compra
  em andamento primeiro".
