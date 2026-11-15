# Plano do funil — 15/11/2026

Meta: o Erick recebe mais avisos de skins que cabem no saldo e se confirmam, porque o app lê mais anúncios
novos por hora, em mais skins, com preço de raro apoiado em prova, e ele vê no painel o que aconteceu com cada
aviso.

Base medida: `docs/diagnostico-funil-2026-11-15.md`. Pedido do Erick em 15/11: fazer tudo menos o Telegram.
Cada etapa é um PR próprio, com teste que roda sem rede. Ninguém junta PR nem instala nada no app em uso.
Banco do app em uso só para leitura (`E:/Documentos/ProjetosIA/SkinSniperV2/app/data/sniper.db?mode=ro`);
nunca ler valor de chave/token em `settings`; nenhum tráfego contra a Steam durante a construção.

## Ordem

- Conversa 1: A (agenda) e depois B (ritmo). As duas mexem em `src/hunt/scheduler.rs`.
- Conversa 2: D (preço do raro) e depois C (catálogo). As duas mexem na prova por pattern. C não toca em
  `scheduler.rs` nem `value.rs`.
- Conversa 3: E (painel).

## A. Agenda

Primeiro investigar, depois mexer. Explicar com dado gravado (`reads`, `arm_stats`, `research_finds`,
`src/hunt/agenda_sim.rs`) por que a agenda concentra leitura, comparando agenda e rodízio cego de forma justa:
mesmos alvos e mesmo tempo desde a última leitura da faixa. Mudar só o que o dado provar.

Aceite: replay sobre as leituras gravadas mostra anúncio raro novo dentro do saldo por leitura da agenda não
abaixo do rodízio cego. Se o replay não conseguir comparar de forma justa, isso é dito e nada muda.

Releitura rápida depois de anúncio novo: só se o dado gravado mostrar que anúncios novos chegam agrupados no
tempo. Se não mostrar, não é construída e o relatório diz isso.

## B. Ritmo

O Erick aprovou em 15/11 o "teste controlado baixando o descanso de 28 s aos poucos"; isso vale no lugar do
teto de ~100/h só para este teste limitado. O risco documentado (bloqueio da Steam em ritmos mais altos) vai
dito a ele em palavras simples.

- Descanso entre leituras de anúncios começa em 28 s.
- Desce 2 s só depois de 6 h sem nenhum 429 em anúncios.
- Nunca abaixo de 20 s (cerca de 150 leituras/h, faixa que rodou 3 a 4 h limpa em 10/11 segundo `projeto.md`).
- Qualquer 429 volta para 28 s por 24 h.
- O espaçamento de 12 s entre leituras e de 3 s global não muda.
- Resposta com `data` nula: primeiro achar a causa no log e nas tabelas; a regra de descanso só muda se a
  causa for achada.

## D. Preço do raro

A queda para o desgaste vizinho documentada em `projeto.md` fica. Guardas candidatas: mais de 3 vendas do grupo
antes de avisar; trava contra múltiplo enorme em skin de centavos; StatTrak/Souvenir separados do normal.

Cada guarda é medida em dado gravado separado no tempo (regra ajustada só com o que veio antes do corte,
medida só no que veio depois). Cada anúncio que a regra de hoje aprovaria cai em uma só de três classes,
testadas nesta ordem (a primeira que servir vence):

1. **inconclusivo**: a faixa não foi relida dentro das 48 h, ou os dois sinais abaixo aparecem juntos (sumiu
   como o mais barato *e* houve anúncio do mesmo grupo abaixo do preço de venda previsto).
2. **provável falso**: 48 h depois continuava anunciado pelo mesmo preço ou menor, ou apareceu anúncio do mesmo
   grupo abaixo do preço de venda previsto.
3. **provável achado**: era o mais barato da faixa, continuou o mais barato até sumir, sumiu em até 48 h e não
   voltou. É indício de compra, não prova: sumir é comprado *ou* retirado.

O que não couber em nenhuma das três também é inconclusivo.

Uma guarda entra só se, depois do corte, tirar mais "provável falso" do que "provável achado". Inconclusivo não
conta para nenhum lado e é relatado à parte. Se os casos depois do corte forem menos de 10, a guarda não entra
por esta regra: fica escrita como "não decidido" até os rótulos finais (16/11 e 20/11). Os números vão no PR.

## C. Catálogo

Medir onde as skins da tabela de patterns caem (tabela → tem venda → comprovada → escolhida) e alargar na maior
queda. Escrever em `projeto.md` como um candidato entra e como é reavaliado. "Preço atual acima do saldo" não é
motivo para tirar alvo. Caça por float continua desligada (decisão do Erick de 12/11).

Aceite: mais skins/seeds escolhidos do que os 21/265 de hoje, com a contagem antes e depois no PR; se a medida
mostrar que não há candidato que passe na regra de entrada, isso é dito e o catálogo não cresce.

## E. Painel: o que aconteceu com cada aviso

Lista de cada aviso, feita de registro que não se perde (`ghosts` com `approved = 1`, `opportunities`).

Estado do aviso no Windows/Telegram, um de:

- **pedido aceito pelo Windows** (o sistema aceitou; não prova que apareceu na tela);
- **falhou** (com o motivo);
- **pendente** (ainda não tentado);
- **desconhecido** (aviso de antes de este registro existir).

O que aconteceu com o anúncio, um de: "saiu do ar em N min (comprado ou retirado)", "ainda anunciado",
"deixou de compensar", "desconhecido (faixa não foi relida)". Mais a conferência de 48 h: "o grupo teve venda",
"não teve", "ainda sem dados". O texto nunca diz que aquele item vendeu nem que deu lucro.

Prova do aviso na tela: botão "Testar aviso" no painel e, depois dele, "apareceu" / "não apareceu"; a resposta
fica gravada com data e aparece no painel. Só o Erick consegue ver a tela, então esta parte é dele.

## Aceite de ponta a ponta (depois de instalado, 7 dias)

O painel mostra estes sete números ao lado da base de hoje; só há melhora se os sete forem cumpridos:

1. Anúncio raro novo dentro do saldo por leitura: agenda não abaixo do rodízio cego no mesmo período
   (o rodízio de 1 em 8 continua ligado como controle). Base: 5 % contra 36 %.
2. Anúncios raros novos dentro do saldo por dia: mediana de 7 dias acima de 43 (o topo da base de 12 a 43).
3. Todo aviso depois da instalação com estado gravado (nenhum "desconhecido") e teste de tela respondido
   "apareceu".
4. Avisos por dia: mais do que a base (14 avisos em 9 dias, cerca de 1,6 por dia), ou seja, 12 ou mais nos
   7 dias.
5. Conferência de 48 h: parte dos avisos com venda do grupo acima da base (3 de 14). Só entram na conta os
   avisos que já completaram 48 h; os mais novos aparecem como "ainda sem dados" e ficam fora da conta, e o
   número é fechado 48 h depois do sétimo dia. Com menos de 10 avisos conferidos o painel mostra a contagem e
   diz que a amostra é pequena, em vez de concluir.
6. Anúncios novos lidos por hora (todos, não só os raros dentro do saldo): acima da base de 14/11 e 15/11
   (286 em 24 h e 148 em 19 h, ou seja, 8 a 12 por hora).
7. Preço nas vendas do grupo em 48 h: para cada aviso conferido, a mediana das vendas do grupo nas 48 h é
   comparada com o preço de venda previsto. "Sustenta" se ficar em 90 % do previsto ou mais; "não sustenta"
   se ficar abaixo; "sem prova de preço" se o grupo teve menos de 3 vendas nas 48 h. Melhora só conta se
   houver 5 avisos ou mais com preço conferido (3 vendas ou mais cada) e a maioria deles sustenta; abaixo
   desses mínimos o resultado é inconclusivo.

Isto só pode ser medido depois que o Erick instalar; antes disso o relatório diz "não medido". Lucro real só
se prova com compra e venda, que ainda não houve.

## Fora

Telegram (o Erick ainda não criou o bot). Segunda saída de rede. Religar caça por float ou de comum.
