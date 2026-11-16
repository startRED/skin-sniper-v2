# Diagnóstico do funil — 15/11/2026

Medido no banco do app em uso (`app/data/sniper.db`, só leitura), em 15/11 às 19h25 de Brasília.
Pergunta: por que o app quase não acha oportunidade. Nada aqui é número de mercado inventado; o que não foi
medido está dito como não medido.

## Funil por dia

| dia | leituras de anúncios | anúncios novos | novos raros dentro do saldo | idade mediana ao ver | achados |
|---|---|---|---|---|---|
| 10/11 | 3 128 | 3 639 | 422 | 7,6 min | 3 |
| 11/11 | 2 283 | 2 321 | 616 | 10,8 min | 2 |
| 12/11 | 1 817 | 621 | 149 | 107 min | 0 |
| 13/11 | 2 681 | 996 | 229 | 38 min | 7 |
| 14/11 | 1 071 | 286 | 43 | 126 min | 1 |
| 15/11 (até 19h) | 1 415 | 148 | 12 | 35 min | 0 |

Saldo: R$ 41,91. 12/11 e 13/11 incluem a primeira leitura de cada faixa de seed (todo anúncio parado conta como
novo); o regime depois disso é de 12 a 43 anúncios raros novos dentro do saldo por dia. Em 9 dias: 116 869
avaliações, 15 aprovadas (todas de raro), 0 compras, 0 vendas registradas.

## O que foi medido

1. **Agenda pior que o rodízio cego.** Últimos 3 dias, tabela `reads`: a agenda (`control = 0`) fez 4 873
   leituras, 246 anúncios novos, ganho 5 633; o rodízio cego (`control = 1`) fez 674 leituras, 246 anúncios
   novos, ganho 2 929. Por leitura: 5 % contra 36 % de anúncio novo e 1,16 contra 4,35 centavos de ganho.
   Em 24 h, dois alvos levaram 60 % das leituras: SSG 08 Acid Fade (765 leituras, 3 anúncios novos) e Glock-18
   Moonrise (433 leituras, 15 novos). O Acid Fade soma 1 675 consultas e 3 achados; os 3 viraram "deixou de
   compensar" em até 96 min (um anúncio mais barato do mesmo grupo apareceu).
2. **Leitura chega tarde.** O anúncio tem mediana de 35 a 126 min quando o app o vê. Os achados que saíram por
   compra/retirada sumiram 144 s, 161 s e 405 s depois de vistos. A Moonrise T1 Center Star FT de R$ 7,20
   (seed 694, 10/11 20:16) foi vista uma vez, já com 66 min; a leitura seguinte do alvo foi 5 h depois.
3. **Teto de leituras.** Descanso fixo de 28 s: 104 a 123 leituras/h. Em 24 h: 6 respostas 429 e 226 erros
   (12 %) em anúncios, quase todos `data` nula (`conhecimento.md` 1.3.1). O teto de ~100/h foi pedido pelo
   Erick (`projeto.md`, Caça item 3) e não é número seguro medido.
4. **Catálogo estreito.** 21 skins e 265 seeds "que pagam". `csfloat_sales` tem 981 nomes, 247 com grupo raro.
5. **Preço do raro bate com o que some, na mediana.** Para 423 anúncios raros que eram o mais barato da faixa e
   sumiram em até 48 h sem voltar: líquido previsto ÷ líquido do preço em que sumiu = 0,83 / 0,97 / 1,00
   (P25 / P50 / P75); 4 % acima de 1,2. O erro grosseiro não é a regra. O ponto fraco é a cauda que vira aviso:
   aprovação com 3 vendas do grupo (ex.: MAC-10 Snow Splash BS a R$ 0,34 prevista a R$ 11,84 com "3 vendas,
   20,8x a 56,4x"; a única venda do seed 309 no CSFloat foi de US$ 4,00 sobre base de US$ 0,11). Em 11 dos 14
   avisos a conferência de 48 h não achou venda do grupo (`ghosts.fast_closed`).
6. **Sem prova final ainda.** 36 compras virtuais, todas `aguardando`: o rótulo sai 10 dias (comum) ou 14 dias
   (raro) depois; os primeiros vencem em 16/11 e 20/11. `purchases` e `outcomes` vazias.
7. **Aviso só no PC.** Telegram sem chave; os 13 avisos saíram pelo Windows. A saída extra (outro IP) não é
   usada desde 12/11: todas as tentativas das últimas 24 h foram pela rota principal.

## Não medido

- Quanto a Steam tolera de leituras por hora neste IP.
- Se os avisos teriam vendido pelo preço previsto (falta o rótulo final).
- Se o robô antigo usa o mesmo IP hoje (nenhum processo dele foi visto neste PC).
