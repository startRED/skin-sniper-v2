# Mina de ouro: aprendizado que evolui sozinho — desenho (06/11/2026)

## Pedido do Erick (palavras dele)

> "Lucro real, achar bastante skins sendo elas extremamente lucrativas. Esse é o meu objetivo, tipo aquela Glock
> Moonrise StatTrak que tinha achado, e até melhores, por exemplo uma Desert Eagle pattern blue gem ou algo
> assim, mas isso é só depois. Eu quero que o próprio Skin Sniper evolua muito rapidamente e fique o mais preciso
> e rápido/eficiente possível para conseguir o máximo de lucro por skin."

Decisões dele, por escolha em cards, em 06/11:

- **Fluxo:** o app avisa e o Erick compra. A compra automática vem depois, quando ele decidir. Ela não entra
  neste desenho.
- **Meta:** lucro real.
- **Aprendizado:** só com sinais grátis. Não haverá compras de teste para o app aprender.
- **Gasto mensal:** nenhum. Sem proxy pago, sem API paga, sem conta nova.

## O que os dados dizem (medido em 06/11, só leitura)

Fontes:
- uma cópia do `app/data/sniper.db` de produção;
- o log de eventos do robô antigo (`Skin Sniper/original/data/events`): 7 dias de set/out, 62 alvos, 363 mil
  observações de anúncio, 10 mil sumiços e 3 vendas reais;
- uma consulta avulsa à Steam.

1. **O v2 joga fora a matéria-prima do aprendizado.**
   - `evaluations` está vazia em produção. A tabela nasceu depois do último anúncio avaliado, porque desde então
     a leitura de anúncios ficou barrada.
   - `purchases` e `outcomes` estão vazias.
   - `listings_seen` guarda só 3 dias (4 850 anúncios), e existem 2 oportunidades no total.
   - O livro de ofertas lido fica só em memória.
   - Sem histórico guardado, não existe aprendizado de longo prazo.
2. **Skin comum quase nunca dá margem contra o próximo anúncio.**
   - Para vender um centavo abaixo do próximo anúncio e ainda pagar a taxa, o anúncio novo precisa estar
     **≥ 13 %** abaixo dele.
   - Para passar na régua do v2 (lucro ≥ R$ 1 e margem ≥ 10 %), precisa estar **≥ ~21 %** abaixo.
   - Medição: 1 463 anúncios de skins comuns que nasceram entre duas consultas (40 alvos, 7 dias).
     Desses, **3 (0,2 %)** estavam ≥ 13 % abaixo, e **nenhum** ≥ 20 %.
   - No v2 (04–06/11): 2 oportunidades em 9 124 consultas. Recalculando a régua nos 4 850 anúncios vistos,
     o mais perto ficou a 6,3 pontos.
3. **O único lucro grande medido veio de padrão.**
   - Glock Moonrise FT seed 59 (T1 Center Star): comprada a ~R$ 5,30 e vendida a R$ 14,39 em < 2 h (≈ R$ 7 de
     lucro depois da taxa, ~135 %).
   - As outras seeds T1, anunciadas na mesma época a R$ 16,7–22, ficaram paradas.
   - Medido no CSFloat em 09/09: fade só paga no top ~5 do rank, e no Moonrise o float dentro do mesmo desgaste
     não pagou nada. Em outras skins, o float de elite saiu a 1,3–2,8× no CSFloat (`projeto.md`).
   - **O prêmio de raridade existe para alguns grupos e não para outros.** Ele tem de ser medido grupo a grupo,
     e anúncio parado não prova preço.
4. **"Sumiu" é quase sempre venda.**
   - Dos anúncios que sumiram no log antigo, só **4 %** voltaram com o mesmo item (`asset_id`) em outro anúncio.
     Quase sempre voltaram mais baratos: o vendedor baixou o preço.
   - Descontados esses, sumir é venda ou retirada.
   - O v2 ainda não lê o `asset_id`, mas a resposta da Steam traz o campo `asset` (fixture
     `steam_listings_moonrise_sorted_price.json`).
5. **O `listingid` não é um relógio.**
   - Os ids vão de 2×10¹⁵ a 9×10¹⁸, e em 19 % dos vizinhos o tempo anda para trás.
   - A idade de um anúncio só sai de "não estava na consulta anterior que cobria esse preço".
   - No robô antigo, a idade ao ser visto foi p50 3 min e p90 11 min.
6. **O livro de ofertas traz a escada inteira.**
   - O `orderbook` devolve `rgCompactSellOrders` (`[preço, quantidade, …]`, ~59 degraus), `cSellOrders` e o mesmo
     para compra.
   - O v2 usa só o menor preço. Com a escada, uma leitura do livro mostra quanto o mais barato está abaixo do
     segundo.
   - Comparando dois livros seguidos, dá para ver quais degraus foram consumidos (vendas).
   - O livro fica num balde de limite separado da leitura de anúncios (PR #58).
7. **`/market/recent` não serve.** Respondeu 200, mas são 10 anúncios por chamada, de todos os jogos da Steam
   (2 de CS2 na amostra).
8. **A cota manda.**
   - Umas 3–10 consultas por minuto num IP.
   - A leitura de anúncios ficou barrada ~45 % do tempo até o PR #58.
   - A previsão de preço já está certa no centro: sem viés, erro médio de 6,6 % em 7 525 casos.
9. **O Telegram não está configurado no app** (`keys.telegram.set = false`). O aviso no celular, que é o fluxo
   escolhido, hoje não sai.

## Consequência: duas teses, cada uma com o seu balde

- **Comum (margem pequena, rara, disputada):**
  - vigiada pelo livro de ofertas, que é barato e tem balde próprio;
  - os anúncios só são lidos quando o livro mostra o mais barato bem abaixo do segundo ou do teto.
- **Exemplar raro (padrão, fade, float de elite; onde mora o lucro grande):**
  - só a leitura de anúncios mostra seed e float, então esse balde vai prioritariamente para cá;
  - o valor do exemplar é aprendido de venda que fechou de verdade, nunca de anúncio parado.

O aprendizado deixa de perguntar só "onde consultar" e passa a perguntar também "quanto vale este exemplar",
"quanto tempo um achado dura" e "quantos anúncios novos chegam por hora".

## Objetivo

Cada consulta vai para onde é mais provável aparecer lucro **que fecha**. Cada aviso traz um valor aprendido de
vendas reais. O app mede sozinho o próprio acerto (lucro virtual depois de 7 dias) e se reajusta todo dia, sem
ninguém mexer em filtro.

## Não objetivos

- Compra automática. Fica para quando o Erick decidir.
- Gastar dinheiro: proxy, API paga, conta nova, compra de teste.
- Mais consultas na Steam ou rajadas. O ritmo continua sendo do limitador: mínimo de 3 s, baldes do PR #58.
- Rede neural ou crate nova sem motivo escrito. Tudo cabe em Rust com a biblioteca padrão e SQLite.
- Mexer na previsão de preço do comum, que já foi medida sem viés.
- Blue gem e padrões caros fora do saldo. O desenho deixa a porta aberta: grupos novos entram pelo coletor do
  CSFloat. Não há trabalho dedicado a eles agora.

## Anti-objetivos (falharia mesmo "funcionando")

- **Aprender com o próprio palpite.** Tratar lucro estimado como lucro real faz o app reforçar os próprios
  erros.
- **Avisar o que não fecha.** Um aviso falso custa o dinheiro e a confiança do Erick. Um aviso a menos é melhor
  que um aviso errado.
- **Confiar em amostra pequena.** Um grupo com 2 vendas não vira "prêmio de 3×". Pouco dado puxa para o neutro.
- **Banco que cresce sem limite.** Toda tabela nova tem retenção.
- **Mudança que não dá para conferir.** Todo ajuste automático fica registrado: o que mudou, de quanto para
  quanto e por quê.

## Desenho em fases

Cada fase entrega algo útil sozinha, e cada uma usa o que a anterior guardou.

### Fase 0 — Guardar a matéria-prima e montar o placar

1. **Registro de observações.** Cada anúncio visto guarda:
   - `listing_id`, `asset_id`, skin, preço, seed, float, paint index e tarefa;
   - primeiro e último instante visto;
   - o intervalo da consulta anterior que cobria o preço dele (a idade máxima).

   **Sumiço:** o anúncio some quando uma consulta seguinte da mesma tarefa cobre o preço dele (mesma regra do
   `mark_gone`: página confiável e preço ≤ maior preço da página) e ele não aparece. Se o mesmo `asset_id`
   reaparecer em outro anúncio em até 24 h, o sumiço vira "relistado".

   **Retenção: 60 dias.** Hoje são 3; os números do item 2 da medição dão a ordem de grandeza.
2. **Registro do livro.** Cada leitura guarda os 5 primeiros degraus de venda, o maior pedido de compra e
   `cSellOrders`/`cBuyOrders`. Só grava quando algo nesse topo mudou. Retenção de 60 dias.
3. **`evaluations`.** Passa a guardar 60 dias, e não mais as últimas 10 000.
4. **Placar diário** (tabela + painel):
   - consultas por balde e horas barradas;
   - anúncios novos vistos e idade mediana;
   - achados e quase-achados (a ≤ τ da régua);
   - sumiços rápidos e lucro virtual (Fase 1).

   É o "antes e depois" de tudo o que vem depois. A linha base é a de hoje: 2 achados em 9 124 consultas.
5. **Uma sonda.** Uma leitura de anúncios com `count` maior que 20, respeitando o espaçamento, para saber se a
   Steam devolve mais anúncios por consulta. Se devolver, a leitura de raros rende mais por consulta. O resultado
   vai para `docs/projeto.md`.
6. **Pré-requisito do Erick, não código.** Configurar o token do Telegram no painel. Sem isso não há aviso no
   celular.

### Fase 1 — Professor grátis (rótulos sem comprar)

Cada aviso e cada quase-achado ganha um resultado medido pelo mercado, não pelo palpite do app.

**Por que existe:** "mal precificada" é o julgamento do app, não um fato. No robô antigo, a P2000 Amber Fade
parecia barata, foi comprada a R$ 4,15 e vendeu 11 dias depois a R$ 3,25 (prejuízo de R$ 1,32: o prêmio
era fantasma). Além disso, a Steam trava por 7 dias o que é comprado no Mercado: todo anúncio traz
`market_marketable_restriction: 7`. O lucro só é garantido se o preço não cair nesse prazo. O professor
confere cada julgamento para o app parar de avisar o tipo que não fecha.

**O rápido vem primeiro:**
- o sumiço responde em minutos;
- a venda de exemplares do mesmo grupo (raro) responde em horas a dias;
- a conta do dia 7 é só a confirmação final, porque é o primeiro dia em que a Steam deixa vender.

A Fase 4 aprende com o sinal rápido assim que ele chega.

1. **Sumiu rápido = alguém comprou.**
   - Sumiço sem relistagem registra o tempo de vida do anúncio.
   - Para comum, os degraus consumidos no livro também contam: um degrau que some entre duas leituras e cujo
     preço não reaparece.
   - Mede a concorrência ("um achado dura X min") e dá um rótulo de demanda por faixa de preço.
2. **Carteira fantasma (comum).**
   - Todo aviso e todo quase-achado vira uma compra virtual pelo preço do anúncio.
   - Passados os 7 dias da trava, o app calcula por quanto venderia e quanto sobra depois da taxa. O preço de
     venda é o menor entre a mediana de vendas dos dias 7–9 (`price_points`) e o menor anúncio do livro no dia 7.
   - Isso é o **lucro virtual realizado**, que fica no placar.
   - **O rótulo não gasta o balde de anúncios.**
     - Usa o histórico que o catálogo já coleta e uma leitura do livro (outro balde) entre os dias 7 e 9.
     - Sem nenhum dos dois até o dia 12, o rótulo fecha como "sem rótulo". Esses casos são contados à parte e
       nunca somem calados.
3. **Venda fechada por exemplar (raro).**
   - O coletor do CSFloat já existe (`patterns.rs`) e passa a guardar todas as vendas limpas com os atributos
     (seed, rank de fade, float) por 60 dias, não só as de 2× para cima.
   - **O rótulo do raro é "fechou ou não fechou"**, medido pelo mercado e nunca pelo prêmio que o próprio app
     estimou.
     - Fechou = entre os dias 7 e 14 depois da compra virtual, algum anúncio do mesmo grupo sumiu sem
       relistagem a um preço ≥ o preço de venda sugerido, ou o CSFloat registrou venda do grupo a um múltiplo
       ≥ o sugerido.
     - O lucro virtual do raro aparece no placar, mas a Fase 4 só usa a taxa de "fechou".
4. **Compra do Erick, quando houver.** `purchases`/`outcomes` entram como o rótulo mais forte, e nunca são
   exigidos.

### Fase 2 — Valor do exemplar aprendido (o ouro)

Para cada grupo raro (tabelas de `groups.rs` + grupos aprendidos), o app aprende o **prêmio que fecha na Steam**
como múltiplo da mediana comum da skin.

- **O prêmio é um múltiplo,** sempre sobre a mediana comum da mesma skin na mesma plataforma e na mesma época,
  como o código de hoje já faz. Por isso não precisa de fator de conversão entre Steam e CSFloat. O ≈ 1,4× de
  09/09 só importa para comparar reais nominais, e fica fora do cálculo.
- **Do CSFloat:** vendas fechadas com o mesmo atributo.
- **Da Steam:** anúncios do grupo com prêmio que **somem em até 48 h** (venda) contra os que ficam parados
  (fantasma). Isso responde direto "o Center Star fecha a 2,7×, as outras T1 não".
- **Seed, depois faixa, depois neutro:** o prêmio é estimado por seed (ou rank de fade) e encolhido para o da
  faixa (ex.: T1). O da faixa, por sua vez, é encolhido para 1×. Com poucas vendas da seed, vale o da faixa;
  com poucas da faixa, vale perto de 1×.
- **Aviso com selo (decisão do Erick):** o raro continua avisando pela regra de hoje, com mediana do múltiplo e
  ≥ `RARE_MIN_SALES` vendas. Todo aviso de raro leva um selo:
  - **comprovado:** o limite de baixo (p25) do prêmio aprendido, com ≥ 10 vendas fechadas na seed ou na faixa,
    ainda passa na régua;
  - **ainda não comprovado:** o resto, com o número de vendas que sustentam o preço. O Erick decide se compra.
- **Float de elite** entra como qualquer grupo: fica se o prêmio medido fecha na Steam, e perde as consultas
  se não fecha.
- **Preço de venda sugerido para o raro:** abrir perto do próximo grupo de anúncios que fecha e descer aos
  poucos. É a lição do Center Star: vender abaixo do mais barato vendeu rápido demais.

### Fase 3 — Agenda de colheita (ver mais e chegar antes)

Troca o índice do UCB1 por "quanto vale olhar este alvo agora", aprendido por alvo e por balde:

- **λ:** anúncios novos por hora (do registro da Fase 0).
- **p:** chance de um anúncio novo ser achado. Vem da cauda de proximidade (τ, já existente) por alvo, encolhida
  para a do grupo de preço.
- **E:** lucro de um achado. No raro, vem da Fase 2.
- **L:** quanto um achado dura antes de sumir (Fase 1).
- **Valor de olhar depois de Δ sem olhar:** `λ · p · E · L · (1 − e^(−Δ/L))`, por consulta do balde.
  - Cresce com o tempo sem olhar e satura quando os achados velhos já foram levados.
  - Responde de uma vez "quem olhar" e "de quanto em quanto tempo".
- **A cota de exploração continua:** alvo pouco testado sempre tem vez.
- **Entrada em sombra, depois teste em dias alternados.**
  - Enquanto o registro tem menos de 14 dias, a colheita só calcula e registra o que escolheria. O UCB1
    continua decidindo.
  - Depois, dias pares ficam com o UCB1 e dias ímpares com a colheita. Vence quem tiver mais quase-achados e
    achados por consulta em 14 dias. O placar mostra a comparação.

Divisão dos baldes:
- **Comum:** lido pelo livro, olhando a escada (o mais barato contra o segundo e contra o teto). Os anúncios
  só são lidos quando passa nesse filtro.
- **Raro:** leitura de anúncios com filtro de seed, como hoje, agora priorizada pelo valor acima.
- **Busca (10 skins por chamada):** varredura larga do catálogo fora da vigia. Mostra o menor preço de muitas
  skins por consulta e acha onde vale abrir vaga.

### Fase 4 — Recalibração contínua e boletim

Começa quando houver ≥ 14 dias de rótulos da Fase 1. Antes disso, não há o que repetir.

- **Todo dia,** um ajuste lê o registro e recalcula τ por faixa, λ, L, a tabela de prêmios e os limiares
  (tolerância do filtro, quase-achado).
- **Os limiares são escolhidos por repetição ("replay")** sobre os anúncios guardados: vence o que daria mais
  lucro virtual realizado.
- **Toda mudança vai para o log e o painel** (o que mudou, de quanto para quanto e por quê), com limite de passo
  por dia para não oscilar.
- **Boletim semanal no painel:**
  - lucro virtual realizado, achados, acerto (aviso que fechou) e horas barradas;
  - os melhores alvos e grupos;
  - tudo comparado com a semana anterior e com a linha base.

## Como saber se está melhorando

| Medida | Linha base (06/11) | Direção |
|---|---|---|
| Lucro virtual realizado por semana (Fase 1) | não existe | sobe |
| Achados por 1 000 consultas | 0,22 (2 em 9 124) | sobe |
| Avisos que fecharam (sumiram rápido ou deram lucro virtual) ÷ avisos | não existe | sobe |
| Grupos raros com prêmio medido (≥ 10 vendas) | 0 | sobe |
| Horas por dia com a caça barrada | ~45 % (antes do PR #58) | cai |
| Idade mediana do anúncio ao ser visto | não existe no v2 (antigo: 3 min) | cai |

## O que dá para esperar

- **Comum:** o ganho é pequeno e disputado. 0,2 % dos anúncios novos pagam sequer a taxa contra o próximo.
  O aprendizado melhora a mira e corta o desperdício de cota, mas não cria margem que o mercado não dá.
- **Raro:** é onde existe lucro de 100 %+ (Center Star: ~135 %). Com quanta frequência isso aparece no saldo
  atual ainda não se sabe. Esse número sai da Fase 0/1 em ~2 semanas de registro.
- **"100 % em toda skin, várias por dia":** nenhum dado medido sustenta isso hoje. O desenho mede, aprende e
  mostra o resultado real, para a decisão de crescer (saldo maior, compra automática) se apoiar em lucro
  medido.

## Limites conhecidos

- O prêmio raro na Steam é cego no histórico: a mediana horária esconde a venda rara em item de alto giro. Por
  isso vem do CSFloat e do sumiço na Steam, e os dois têm ruído.
- Sumiço por retirada sem relistar conta como venda. Pelo log antigo, a relistagem com o mesmo item é 4 %, e a
  retirada pura não dá para medir.
- O fator Steam ÷ CSFloat (≈ 1,4) foi medido em 3 itens e é re-medido continuamente.
- Com a leitura de anúncios barrada, a Fase 2 aprende mais devagar: só o CSFloat e o sumiço observado.
- A carteira fantasma do comum supõe vender pela mediana dos dias 7–9 ou pelo menor anúncio. Não modela o
  tempo até vender.

## O que fica com quem constrói

- Esquema das tabelas, nomes e forma dos testes.
- Como compactar o registro do livro.
- Onde o placar aparece no painel.

Os números (60 dias, 5 degraus, 48 h, p25, 24 h para relistagem, 10 vendas, limite de passo diário) são
hipóteses de calibração e vão escritos assim no `projeto.md`.

## Decisões do Erick (06/11, depois da revisão)

- Raro continua avisando enquanto o app aprende, com selo "comprovado" / "ainda não comprovado".

## Decisões minhas (o Erick pode trocar)

- Ordem das fases: 0 → 1 → 2 → 3 → 4. Registro e placar primeiro, porque sem eles nada se mede.
- Retenção de 60 dias, selo "comprovado" pelo p25 com ≥ 10 vendas, 48 h para "fechou" no raro e 24 h para
  relistagem.
- Prêmio como múltiplo na mesma plataforma, sem fator de conversão. Estimado seed → faixa → neutro.
- Rótulo do raro binário ("fechou"). O lucro virtual do raro é só informativo.
- Rótulo do comum sem gastar o balde de anúncios. Sem dados até o dia 12, fecha como "sem rótulo".
- Colheita em sombra até 14 dias de registro, depois dias alternados contra o UCB1.
- Fases 2 e 3 construídas logo após a 0 e a 1, porque usam dados que já existem (CSFloat) ou entram em sombra.
  A Fase 4 espera os rótulos.
- Busca como varredura larga só na Fase 3.
- Boletim no painel, não no Telegram. Um tipo novo de notificação já quebrou as listas do Telegram no robô
  antigo.
