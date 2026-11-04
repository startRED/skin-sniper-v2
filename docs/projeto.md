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
GET  /api/opportunities    -> Opportunity[]         (ativas, ordem livre; o frontend ordena)
GET  /api/opportunities/:id/history -> {points: [[ts, median_cents, purchases]], sell: Sell}
POST /api/buy              {id: string}            -> Purchase
GET  /api/purchases        -> Purchase[]
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
  evidence: string,              // frase curta: "392 vendas/dia nos últimos 7 dias"
  seen_at: number
}
Sell = { high: Tier, mid: Tier, quick: Tier }
Tier = { price_cents: number, net_cents: number, profit_cents: number, eta_hours: number | null }
Purchase = { id, name, icon_url, price_cents, status: "pending" | "need_confirmation" | "bought" | "failed",
             message: string | null, bought_at: number | null, tradable_at: number | null, sell: Sell }
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
2. **Seleção:** vigia itens com piso ≤ saldo e ≥ 3 vendas/dia. Nota = vendas/dia × dispersão relativa
   (P50−P10)/P50, multiplicada por (1 + acertos recentes). Mais as consultas de grupo raro
   (`propertyFilters` por faixas de seed dos tiers) das skins com tabela e piso ≤ saldo.
3. **Ritmo:** um limitador global. Começa com 1 consulta a cada 3 s. Cada 30 min sem 429 encurta 5 %
   (piso 1,5 s); um 429 pausa 60 s (dobrando até 10 min) e alonga o intervalo 50 %. Breaker por endpoint.
   Os números ficam no `State` para o painel mostrar.
4. **Detecção:** cada consulta devolve os anúncios mais baratos; `listingid` novo com preço ≤ saldo vai para
   avaliação; anúncio que sumiu vira evento `gone`.

## Precificação (o coração)

- Taxas: fórmula em centavos de `conhecimento.md` 5.1; inverso por busca binária. Nunca ÷ 1,15.
- A skin comprada fica **7 dias travada**: todo preço de venda é para daqui a 7 dias.
- Base (item comum): pontos horários dos últimos 7 dias de `QueryPriceHistory`, quantis ponderados
  por compras: Q25, Q50, Q75. Tendência: mediana dos últimos 3 dias contra a dos dias 4–14; se cair,
  multiplica tudo pelo fator projetado em 7 dias (piso 0,85); se subir, não credita (fator 1).
- `quick = min(Q25·f, menor outro anúncio − 1 centavo)`, `mid = Q50·f`, `high = Q75·f`.
- Raro: o grupo do anúncio (tier, fase, faixa de fade) busca vendas do CSFloat do mesmo grupo
  (`price / reference.base_price`). Com n ≥ 3: os três preços da base × Q25/Q50/Q75 do múltiplo. Com n < 3:
  usa a base comum e escreve na evidência "prêmio sem prova (n vendas)". Preço pedido nunca vira preço.
- Prazo `eta_hours` = 24 / (vendas/dia × fração do peso com mediana ≥ preço), depois da trava.
- Confiança: alta (≥ 20 vendas/dia e ≥ 100 pontos em 7 dias; raro com n ≥ 8), média (≥ 5/dia; raro n ≥ 3),
  baixa no resto.
- Entra na lista: lucro no `mid` ≥ R$ 0,20 e margem ≥ 5 %. `safe` quando lucro no `quick` > 0.
  Aviso no Telegram/som: `safe` e lucro no `mid` ≥ `notify_min_profit_cents` (padrão R$ 1,00).
- Calibração: cada compra grava os três preços previstos; quando o Erick vender, o realizado entra numa
  tabela para conferir o erro (v1 só grava; o ajuste fino vem depois com dados).

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
  Ordenar clicando no cabeçalho (lucro, margem, lucro seguro, prazo, mais novo). Linha nova entra com
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
