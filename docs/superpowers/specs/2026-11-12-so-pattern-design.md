# Só pattern: catálogo provado por venda e agenda por faixa de seed (12/11/2026)

Pedido do Erick: caçar só pattern raro que valoriza de verdade e tem demanda, sem float e sem skin barata nas
consultas; e refazer o aprendizado para que ele melhore com o tempo sem placebo. O objetivo é lucro.

As regras em vigor estão em `docs/projeto.md` ("Caça" item "Só pattern", "Aprendizado" item 6, "Precificação" item
"Catálogo de pattern"). Este arquivo guarda o porquê e o que foi medido.

## O problema

- A caça gastava leitura de anúncios com float (850 leituras em 24 h, 200 anúncios novos, nenhum achado) e o
  prêmio de float não fecha na Steam (medido em 11/11).
- O catálogo de pattern eram tabelas de guia. Guia diz que o seed é bonito, não que alguém paga por ele: seeds de
  tier 1 do Solitude (95, 112, 154) e do Trace Lock (309, 390) vendem a preço comum. Esses são os alvos que
  aparecem toda hora e não valorizam.
- A agenda aprendia por alvo (skin × desgaste), mas a consulta é por faixa de seed. Um alvo com uma faixa boa e
  dez ruins recebia a mesma nota em todas.
- Nada dizia se o aprendizado ajudava. Com 5 oportunidades em toda a vida do app, lucro realizado não separa uma
  agenda boa de uma ruim.

## O que mudou

1. **Float fora.** Sem grupo, sem alvo, sem prêmio. Venda com float nos 10 % melhores do desgaste também não
   conta como prova de pattern.
2. **Catálogo provado por venda.** As tabelas viram lista de candidatos (`named.json`: 28 grupos em 22 skins,
   `blue.json`: % de azul por seed de 5 skins, mais as que já existiam). Um seed só é raro quando as vendas do
   CSFloat dele, do grupo dele ou de seeds de nota igual ou pior pagam acima do comum (4 regras em
   `hunt/patterns.rs`). Skin medida é consultada só nos seeds pagantes.
3. **Agenda por faixa de seed.** `V = taxa × peso × estoque × lucro`: a taxa ainda é do alvo (há pouca medida por
   faixa), mas peso, estoque e lucro são da faixa. A faixa larga de uma skin de giro alto e prêmio alto é lida
   mais vezes; a faixa de um seed só, de prêmio baixo, quase nunca.
4. **Três conferências contra placebo**, no painel:
   - **controle**: 1 leitura em 8 vai para um rodízio cego (a faixa lida há mais tempo). O placar compara, por
     leitura, o que a agenda e o rodízio trouxeram. É o grupo de controle permanente que programas de
     recomendação e de anúncios grandes mantêm: sem ele, qualquer melhora pode ser só o mercado;
   - **previsão**: cada leitura é prevista só com as anteriores e o erro decide os parâmetros (avaliação
     prequencial, a mesma ideia do teste "andando para frente" de séries temporais);
   - **preço**: cada venda de seed pagante é prevista só com as vendas anteriores a ela.
5. **Sinal denso.** Toda leitura grava `worth` = anúncios novos × lucro da faixa. Achado é raro demais para
   ensinar; anúncio novo em faixa que paga acontece toda hora e é o que precede o achado.

## Medido em 12/11 (banco de produção, só leitura, 45 759 vendas do CSFloat)

Repetir: `SNIPER_DB=<caminho>/sniper.db cargo test --release catalog_on_a_real_db -- --ignored --nocapture`.

| piso comum | skin | seeds pagantes | prova (mediana) |
|---|---|---|---|
| US$ 0,95 | Glock-18 High Beam | 4 | 2,38× |
| US$ 1,01 | Sawed-Off Amber Fade | 8 | 1,88× |
| US$ 1,88 | USP-S Tropical Breeze | 1 | 3,23× |
| US$ 3,10 | P2000 Amber Fade | 5 | 2,49× |
| US$ 3,27 | Desert Eagle Serpent Strike | 2 | 4,81× |
| US$ 4,50 | MP7 Amberline | 5 | 1,88× |
| US$ 4,83 | SSG 08 Acid Fade | 34 | 2,53× |
| US$ 6,44 | Glock-18 Moonrise | 5 | 1,83× |
| US$ 6,86 | Desert Eagle Heat Treated | 104 | 13,80× |
| US$ 7,55 | M4A1-S Solitude | 2 | 4,40× |
| US$ 8,23 | XM1014 Seasons | 1 | 3,01× |
| US$ 8,58 | AUG Amber Fade | 6 | 1,67× |
| US$ 12,76 | Five-SeveN Heat Treated | 51 | 2,98× |
| US$ 13,36 | AWP PAW | 2 | 3,18× |
| US$ 14,31 | AWP Exothermic | 1 | 15,18× |

Total: 15 skins, 231 seeds. O piso é o menor entre os desgastes da skin; a prova é a mediana, entre os seeds, da
mediana das razões que provam cada um.

Prova retroativa: 336 vendas de seed que já pagava; 97 % saíram a ≥ 1,15× o comum; real ÷ previsto 1,12.

## O que não foi medido

- **Se a agenda nova ganha do rodízio.** Só o placar dirá, depois de alguns dias de leitura.
- **Se o lucro aparece na Steam.** A prova é do CSFloat; na Steam o mesmo grupo já saiu a 2,0–2,6× (Center Star)
  e a 1,4× (High Beam T1) contra 1,6× e 2,9× lá. A razão de 13,8× do Heat Treated é de vendas a dinheiro.
- **Os limites da regra** (mediana 1,5×, quartil 1,2×, 2× fora de tabela, 8 vizinhas, 3 vendas): escolhidos
  olhando o mesmo retrato em que a prova retroativa foi medida, então os 97 % são otimistas.
- **`L` = 900 s** para anúncio raro e os padrões `W`/`m`/`H`, medidos em 10/11 na conta antiga.
- **`worth` como sinal de lucro**: é hipótese que anúncio novo em faixa pagante antecede achado.

## Limites conhecidos

- O catálogo ficou menor em skins do que a lista de tabela (15 medidas e pagantes): skin de tabela com menos de
  10 vendas limpas por nome não é medida, logo não é caçada. Ele cresce sozinho conforme o coletor lê vendas
  (uma skin a cada 2 min).
- Faixas acima do saldo ficam fora da busca; facas e luvas ficam fora do catálogo.
- O rodízio lê faixas paradas há mais tempo e por isso acha mais estoque por leitura: a comparação é
  conservadora para a agenda.
- Sobras no código: `Task.float`, `Mode::Float`, a tabela `learned_sales` e a coluna `grp` de `csfloat_sales`
  (gravada pela regra antiga) ficam sem uso.
