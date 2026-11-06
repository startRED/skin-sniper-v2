# Aprendizado da caça — desenho (06/11/2026)

Pedido do Erick: o Skin Sniper deve melhorar sozinho, sem ninguém ajustando filtro. Quanto mais ele roda, mais
consultas vão para os lugares onde aparecem boas oportunidades. Rust é o preferido, e Python só entra se for
necessário. O Erick pulou as perguntas e pediu decisão autônoma; as escolhas abaixo são minhas.

## O que os dados disseram (leitura do `data/sniper.db` em produção, 06/11)

- **A agenda aprende quase nada.** Em 2 dias foram 9 124 consultas em 196 alvos, e só 2 alvos acharam alguma
  coisa (2 oportunidades). Quem decide a agenda é o UCB1, que só ganha nota com achado ou com quase-lucro
  (até R$ 0,30 ou 10 %). Esse sinal é raro demais para separar os alvos bons dos ruins.
- **Cada anúncio visto carrega informação.** Nos mesmos 2 dias, 4 850 anúncios de 136 skins foram vistos.
  Medi a distância de cada um até dar lucro: g = (venda líquida na mediana de 7 dias − preço − lucro mínimo) ÷
  preço. Nenhum passou da régua, e o mais perto ficou a 6,3 pontos. As skins são muito diferentes entre si:
  24 delas tiveram anúncio a menos de 5 % de empatar, e as outras nunca chegaram perto. Esse é o sinal denso
  que faltava.
- **A cauda é exponencial.** Acima do percentil 90 de g, o excesso médio fica entre 2,6 e 4,0 pontos,
  conforme o corte (p95: 2,6; p90: 3,7; p80: 4,0). Na prática, cada ~3 pontos mais perto de dar lucro
  multiplicam a chance por e.
- **A previsão de preço está certa no centro.** Conferi a fórmula de `sell::estimate` (Q50 de 7 dias com
  tendência) contra o que o mercado fez 7 a 9 dias depois: 7 525 casos em 493 skins. Mediana do real ÷
  previsto = 1,000 (p10 0,914, p90 1,104). Metade do volume vendeu a ≥ médio previsto e 70 % a ≥ rápido.
  Erro médio absoluto de 6,6 %. Nenhuma faixa de preço tem viés (< R$ 5, R$ 5–15 e R$ 15–40, todas entre
  0,996 e 1,000). **Conclusão: o preço não é o gargalo, e o aprendizado não mexe nele.**
- **O gargalo é a cota de consultas.** Na prática sai ~3 consultas/min por IP, e a saída extra pela Starlink
  levou 429 desde a primeira consulta. A velocidade do Rust não muda isso: o aprendizado vale pelo que decide
  fazer com cada consulta.

## Objetivo

Cada consulta deve ir para onde é mais provável aparecer um anúncio que passa na régua. O aprendizado usa
todo anúncio visto, não só os achados. O ciclo é contínuo: vigia → aprende → troca o que vigia → vigia.

## Não objetivos

- Mudar a régua de dinheiro: lucro no médio ≥ R$ 1,00 e 10 %, lucro esperado ajustado ao risco > 0, aviso,
  combinação. O aprendizado só decide **onde consultar**, nunca **o que comprar**.
- Compra automática. O plano antigo continua valendo: só depois do 1º flip manual validado.
- Mudar a previsão de preço (medida sem viés acima).
- Python, rede neural, crate nova. Tudo cabe em Rust com a biblioteca padrão.

## Anti-objetivos (falharia mesmo "funcionando")

- Mais consultas na Steam, ou rajadas. O ritmo continua sendo do limitador.
- Uma skin ruim sair da vigia para sempre. Ela tem que voltar a ser testada.
- Alvo novo começar com nota zero e nunca ser consultado.

## Desenho

### 1. Nota por proximidade (cada anúncio ensina)

Hoje cada consulta rende à agenda o lucro das oportunidades novas, mais 15 % do lucro mínimo por quase-lucro
dentro de uma janela fixa. Passa a render:

- **oportunidade nova**: o lucro esperado no preço médio, como já é hoje;
- **cada anúncio novo avaliado que não passou**: `lucro mínimo × e^(g/τ)`, com
  g = (lucro no médio − lucro mínimo) ÷ preço, sempre ≤ 0 nessa conta. Um anúncio a τ de passar vale 37 % do
  lucro mínimo; a 2τ, 14 %; um a −25 % com τ = 3 pontos vale ~0,02 %. A conta é contínua em g = 0 e encaixa
  com o achado.

Só entram anúncios **novos** (o `listingid` ainda não estava em `listings_seen`), que cabem no saldo e têm
estimativa. Reavaliar o mesmo anúncio quando o histórico muda não rende de novo: senão o alvo com anúncio
parado acumularia nota. O contador "quase deu lucro N vezes" do painel continua igual.

O UCB1 continua: determinístico, com cota de exploração de 12,5 % por modo, meias-vidas de 24 h e 4 h e
persistência em `arm_stats`. Só a recompensa ficou densa. Com ela, o alvo consultado a cada minuto vê poucos
anúncios novos por consulta e a nota dele cai sozinha. A frequência de cada alvo se equilibra pelo que rende
por consulta.

### 2. Escala τ aprendida (o v2 mede a própria cauda)

τ = excesso médio de g acima do percentil 90, calculado nas últimas 5 000 avaliações com estimativa da tabela
`evaluations` (o histórico de auditoria que já existe). Recalcula a cada seleção (15 min). Precisa de pelo
menos 200 avaliações e fica entre 1 e 10 pontos. Sem dados, usa 3 pontos (medido acima; hipótese até o v2
juntar as próprias avaliações). Essa é a técnica "picos acima de um limiar" da teoria de valores extremos,
o jeito clássico de estimar a chance de um evento raro a partir dos eventos moderados.

### 3. Seleção que gira o catálogo

A nota de seleção do desconto hoje é `nota-base × (1 + acertos nas últimas 24 h)`. O `(1 + acertos)` vira
um fator aprendido por alvo `d:`:

- referência = mediana de `r/n` (recompensa por consulta, com decaimento) entre os alvos `d:` com pelo
  menos 10 consultas;
- média encolhida do alvo = `(r + 20 × referência) / (n + 20)`: com poucas consultas, ela fica perto da
  referência;
- fator = média encolhida ÷ referência, limitado a [0,25; 4]. Alvo sem consulta, ou referência zero, fica
  com fator 1.

Quem rende pouco perde a vaga (`WATCH_MAX` = 40) para uma skin do catálogo ainda não testada, que entra com
fator 1. O UCB1 testa essa skin logo de cara (índice infinito para alvo novo). O alvo que saiu guarda as notas,
e `n` e `r` decaem pela metade a cada 24 h. Sem consulta nova, a média encolhida volta para a referência e o
fator para ~1 em poucos dias, e ele volta a disputar a vaga. Isso é o "começar sem filtro e ir filtrando"
do Erick, sem tirar ninguém de vez.

Raro e float ficam como estão (seleção por vendas/dia). As tabelas cobrem poucas skins, e não há fila de
candidatos esperando vaga.

## Como saber se melhorou

Dá para medir sem código novo, com `evaluations`, `arm_stats` e `opportunities`. Antes/depois, por dia:

1. oportunidades por 1 000 consultas (antes: 2 em 9 124);
2. fração das consultas gastas em alvos que tiveram anúncio a ≤ 5 pontos de passar;
3. g médio dos 10 % melhores anúncios vistos por dia (deve subir: a caça vai para onde a cauda é mais gorda).

## Limites conhecidos (registrados, não resolvidos aqui)

- A chance de achado só é extrapolada da cauda. Achado de verdade é ~1 em 2 000 anúncios, então a confirmação
  vai levar dias.
- Anúncio bom some rápido (~11 min medidos em 2 oportunidades). A proporção de bons entre os vistos sai menor
  que a real. Para comparar alvos, isso é um fator quase constante.
- No raro, as faixas de seed giram, e a recompensa por consulta mistura as faixas. Fica como está.
- Uma agenda por "valor acumulado desde a última olhada" (modelo de colheita) seria o passo seguinte, se o
  UCB1 com recompensa densa repetir alvo à toa.

## O que fica com quem constrói

Nomes de funções, onde guardar τ no estado compartilhado, a forma dos testes. Os números (5 000, 200, 1–10
pontos, 3 pontos, 20, [0,25; 4], 10 consultas) são hipóteses de calibração e vão escritos assim no
`projeto.md`.
