# Aprendizado unificado: um valor por leitura

Data: 2026-11-10. Pedido do Erick: reformular o aprendizado inteiro, para ele se corrigir sozinho em dias e
achar mais oportunidades. Executado em modo autônomo (sem rodada de aprovação).

## Por que

Medido na produção em 10/11 (banco `app/data/sniper.db`, só leitura):

- A agenda aprendia com "proximidade do lucro" (`learn::closeness_credit`). Esse crédito é ≈ 0 em 99,9 % dos
  anúncios: o sinal não distingue um alvo do outro. Achado de verdade: 3 na vida do app.
- A colheita (`harvest`) estava em sombra e só trocaria de política depois de 14 dias, comparando achados + quase
  por consulta — o mesmo sinal quase nulo.
- O que existe em quantidade é **anúncio novo que cabe no saldo**: 2 694 em 7 100 leituras de anúncios (0,38 por
  leitura). Todo achado é um anúncio novo; quem vê mais anúncio novo por leitura vê mais achado.
- Anúncio novo se acumula entre leituras do mesmo alvo, em escala de horas (comum: 0,06 por leitura com intervalo
  ≤ 3 min; 0,39 em ≤ 63 min; ≈ 5 depois de 2 h).

## Objetivo

Cada leitura da Steam vai para o alvo com o maior **valor esperado em centavos agora**. Um modelo só, com
parâmetros que o próprio app reajusta de hora em hora contra o histórico e registra.

Fora do objetivo: aumentar o volume de leituras (teto de 300/h e piso de 12 s ficam), mudar o critério de
compra/alerta, afrouxar a prova de preço do raro (`RARE_MIN_SALES = 3`), comprar qualquer coisa.

Teto honesto: ≈ 1 achado a cada ~3 400 anúncios novos. O modelo aumenta anúncios novos por leitura e para de
gastar leitura onde nada chega; não cria achado onde o mercado não oferece.

## O modelo

Alvo `k`, classe `c` = prefixo do id (`d` desconto, `r` raro por seed, `f` faixa de float):

```
V_k = taxa_k × L·(1 − e^(−Δ_k/L)) × retorno_k
```

- `Δ_k` = segundos desde a última vez que a agenda olhou o alvo (`max(last_at, picked_at)`): leitura vazia ou com
  falha também zera o estoque, senão o alvo que falha segura a frente. `L` = janela de decisão: por quanto tempo
  um anúncio barato fica na página antes de sumir.
- **Taxa** (anúncios novos por segundo de exposição):
  `a = r_k + m·W·λ_c`, `taxa = (a + z·√a) / (exposição_k + m·W)`.
  `r_k` = anúncios novos do alvo; `exposição_k` = Σ `W·(1 − e^(−intervalo/W))` das leituras (a primeira conta
  `W`); as duas decaem com meia-vida `H`. `λ_c` = Σr ÷ Σexposição da classe (o alvo novo começa na média dos
  irmãos). `z·√a` é o otimismo: incerteza de contagem, some com medida.
- **Teto por seca:** `taxa ≤ 3 ÷ seca`, com seca = exposição somada desde o último anúncio novo do alvo (sem
  decaimento; regra de três: zero evento em `s` segundos dá, com 95 %, taxa ≤ 3/s). Sem ele, com `H = 72 h`, o
  alvo que secou seguia recebendo leitura por mais de um dia (medido na simulação). O primeiro anúncio novo zera
  a seca e a taxa medida volta inteira. Vale na decisão, no motivo do painel e na repetição do ajuste.
- **Retorno** (centavos por anúncio novo): `(N0·w_c + ganho_k) / (N0 + novos_k)`, com
  `w_c = (N0·w_g + G_c)/(N0 + F_c)` e `w_g = ΣG/ΣF` (vida inteira, sem decaimento). Sem nenhum ganho medido,
  retorno = 1 para todos (só a taxa decide).
- Alvo nunca lido vale infinito. `last_at` sobrevive à migração, então não há varredura no arranque: com `r` e
  exposição zerados todos empatam na média da classe e o empate vai para o lido há mais tempo.
- Toda leitura conta, inclusive a que não trouxe anúncio novo (é ela que mede a seca).
- A fatia de exploração (12,5 %, rodízio de modo, gancho da exploração externa) e o piso de revisita ficam.

`boosts` (fator na seleção de alvos comuns) passa a ser `taxa × retorno` do alvo ÷ mediana dos comuns testados,
limitado a [0,25; 4].

## Medida que sustenta os padrões

Perda de Poisson prequencial (prevê cada leitura só com o passado; `μ − f·ln μ`), 7 100 leituras, 2 694 novos:

| Modelo | Perda |
|---|---|
| Constante | 0,5895 |
| Taxa por classe | 0,5521 |
| Taxa por alvo, por leitura (sem intervalo) | 0,4951 |
| Classe + janela W = 7200 s | 0,3992 |
| **Alvo + janela W = 7200 s, m = 1** | **0,3236** |
| Alvo + janela W = 21600 s, m = 1 | 0,3303 |
| Alvo + janela W = 2627 s, m = 4 | 0,3444 |
| W = 7200 s, m = 4, sem decaimento / com 24 h | 0,3324 / 0,3342 |

Padrões: `W = 7200 s`, `m = 1`, `H = 72 h` (decaimento medido como neutro; fica para acompanhar mudança de
mercado). `L` = percentil 25 da vida dos anúncios que eram o mais barato e sumiram sem relistagem (928 s em 607
sumiços; 900 s enquanto faltam 20).

Teto por seca, numa reconstituição mais grossa do mesmo histórico (2 545 leituras juntas, 916 novos atribuídos):
sem teto 0,3563; teto 3 → 0,3551 (encostou em 278 de 2 492 leituras); teto 5 → 0,3565; teto 8 → 0,3563. Não
piora a previsão e corta o gasto no alvo que secou. Meia-vida no mesmo conjunto: 6 h 0,3650; 24 h 0,3575; 72 h
0,3563; 168 h 0,3562 — 6 h ficou fora da grade.

Hipóteses de calibração, **não medidas**: `N0 = 500` anúncios, `z = 1`, `L` = p25 (e não mediana), margem de 1 %
para adotar parâmetro novo, o 3 do teto por seca, retenção de 14 dias de leituras.

## Autoajuste

De hora em hora, só banco:

1. Repete as leituras dos últimos 7 dias (`reads`; páginas do mesmo alvo em até 90 s contam como uma leitura)
   para cada combinação de `W` {1800, 3600, 7200, 14400, 28800} ×
   `m` {0,5; 1; 2; 4} × `H` {24, 72, 168 h} e mede a perda prequencial (a primeira leitura de cada alvo não conta).
2. Troca os parâmetros só se a melhor combinação for ≥ 1 % melhor que a atual e houver ≥ 1 000 leituras pontuadas.
   Registra em `refit_log` (`fluxo:janela`, `fluxo:prior`, `fluxo:meia_vida`).
3. Na troca, reconstrói `r`/exposição de cada alvo com o estado final da repetição.
4. Atualiza `L` e grava o resumo do placar (`settings.value_info`): parâmetros, perda do modelo × perda da taxa
   por classe, previsto × real, por classe.

## Banco

- `arm_stats.fresh` (vida inteira), carregado de `research_credits ⋈ observations`. `r` e `exposure` zerados na
  migração: mudam de unidade (crédito de proximidade → anúncios novos; vida de 600 s → janela `W`).
- `reads(at, arm, fresh)`: uma linha por leitura recompensada; retenção de 14 dias.

## Prêmio por seed

Testei exigir significância binomial (α = 0,01) para um seed virar grupo: derruba 9 dos 12 grupos ativos sem
nenhuma venda contrária — inclusive Desert Eagle Heat Treated a 12–15× — porque a skin tem 33 % de vendas ≥ 2×.
**Não implementado.**

Fica só a correção de evidência contrária: o seed que já tem ≥ 3 vendas altas passa a contar também as vendas dele
a preço normal (`csfloat_sales`, razão < 2×) nos quantis do preço. Hoje não muda nenhum dos 12 grupos (nenhum tem
venda normal registrada); protege quando aparecer.

## Sai

`learn.rs` (proximidade), `harvest.rs` (sombra, dias alternados, `Policy`), o índice UCB1 e a meia-vida de seca.
`research_decisions.scale` passa a gravar 0; `policy` grava `"valor"`.

## Fica

Professor, fantasmas e `teacher::refit`; exploração externa e o ensaio (`trial`); crédito de descoberta; registro
(`ledger`); precificação e critério de compra.

## Como saber se melhorou

- Placar → "Agenda": perda do modelo abaixo da perda por classe; previsto ≈ real.
- Anúncios novos por leitura de anúncios acima de 0,38 (linha de base dos 4 dias antes da troca).
- `refit_log` com chaves `fluxo:*` quando o ajuste trocar parâmetro.

## Decisões minhas (não do Erick)

Sinal = anúncio novo dentro do saldo; forma do valor; padrões e hipóteses acima; teto por seca; zerar `r` e
exposição na migração em vez de reconstituir as leituras antigas (a atribuição histórica é por horário,
imprecisa); descartar o teste binomial.
