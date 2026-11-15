# Conclusão da agenda — 15/11/2026

## Objetivo e decisão

Queremos mais avisos de skins que cabem no saldo e se confirmem: ler mais anúncios novos por hora, em mais skins, apoiar o preço de raro em prova e mostrar no painel o destino de cada aviso. Esta análise trata somente da agenda; não comprova os demais resultados.

A tarefa A exige comparação justa entre os mesmos alvos e o mesmo tempo desde a última leitura da faixa. Os registros disponíveis não permitem reconstruí-la. Portanto, não há mudança de comportamento na agenda, no cálculo de valor, na simulação ou no controle de uma leitura em oito.

## O que foi medido

Banco local aberto somente para leitura; corte fixo em `max(reads.at) = 1794782751`. Nos três dias até o corte, leituras de faixa: agenda 4.870 leituras, 246 anúncios novos e ganho 5.633; controle 675, 245 e 2.929. As medianas dos intervalos conhecidos são 513 e 59.603 segundos, respectivamente. Primeiras leituras de faixa: agenda 29 leituras e 147 anúncios novos; controle 212 e 191. Elas incluem estoque já existente e não medem chegada entre visitas.

Com intervalo conhecido abaixo de 600 segundos, a agenda fez 2.539 leituras e encontrou 9 anúncios novos; o controle não teve leituras. Com intervalo de pelo menos 21.600 segundos, agenda 300 leituras e 41 anúncios novos; controle 456 e 53. Mesmo esse último recorte inclui intervalos diferentes e não é uma comparação justa. No recorte dos mesmos alvos, presentes nos dois controles com intervalo de pelo menos 21.600 segundos, foram 296 leituras e 40 novos na agenda, contra 439 e 53 no controle. A janela de 2 horas aproxima o estoque acima de 6 horas segundo uma suposição do modelo; isso não prova igualdade de tempo nem constitui replay contrafactual. Nos três dias não há pares exatos de alvo e intervalo compartilhados pelos controles; a faixa não é identificada.

A taxa bruta nos três dias foi 0,05 novo por leitura na agenda e 0,36 no controle. As 2.539 leituras rápidas com 9 novos contribuem para essa diferença, mas não demonstram toda a causa. `fresh` representa anúncio novo dentro do saldo, creditado uma vez; no caminho de detecção de raro, o crédito também depende de o anúncio não ser conhecido e passar por `first_credit`.

Nas 24 horas até o corte, agenda 1.715 leituras e 19 anúncios novos; controle 242 e 37; ganho zero para ambos. Os alvos `r:G182820FD013004` e `r:G180420B6053004` tiveram 781 e 435 leituras, totalizando 1.216 de 1.957 (62,1%); os anúncios novos associados foram 3 e 15, respectivamente.

No retrato atual de `arm_stats`, Acid Fade (`r:G182820FD013004`) tem 1.684 consultas, 3 achados, ganho 2.726 e 97 anúncios novos; `research_finds` registra os mesmos 3 achados e ganho 2.726. Esse retrato não é histórico.

## Limites

O código calcula valor pela taxa otimista por alvo, multiplicada pelo peso e ganho da faixa e pelo estoque acumulado (exposição). O prêmio esperado favorece alvos com maior retorno; maximizar retorno monetário é diferente de maximizar contagem de anúncios novos. A concentração observada não demonstra toda a cadeia causal sem os pesos históricos.

`reads` guarda instante, alvo, anúncios novos, ganho, intervalo, peso, controle e crédito monetário observado (`worth`), distinto do valor usado para escolher, mas não identifica a faixa nem registra o conjunto de candidatas e ganhos históricos. `research_decisions` guarda alvo, origem, política, escala e bloco, sem reconstruir esse conjunto. `arm_stats` é apenas o estado atual. A simulação `agenda_sim` gera chegadas Poisson e vidas exponenciais; não é replay contrafactual dos anúncios observados.

Não foi demonstrado agrupamento de chegadas: cada leitura agrega anúncios chegados entre visitas. Por isso, não há base para implementar releitura rápida. Para decidir uma mudança, seria necessário registrar dados suficientes para comparar alternativas elegíveis sob as mesmas condições e avaliar os resultados.

## Consultas reproduzíveis

Execute no SQLite abrindo `file:E:/Documentos/ProjetosIA/SkinSniperV2/app/data/sniper.db?mode=ro` como URI. As consultas não acessam configurações. O limite superior é inclusivo; os inferiores também.

```sql
-- Totais e mediana dos intervalos conhecidos; altere 259200 para 86400 para 24 horas.
WITH r AS (
  SELECT * FROM reads WHERE at BETWEEN 1794782751-259200 AND 1794782751
    AND weight IS NOT NULL
), ranked AS (
  SELECT control, gap, row_number() OVER (PARTITION BY control ORDER BY gap) AS n,
         count(*) OVER (PARTITION BY control) AS total
  FROM r WHERE gap IS NOT NULL
), median AS (
  SELECT control, avg(gap) AS gap_mediana FROM ranked
  WHERE n IN ((total+1)/2, (total+2)/2) GROUP BY control
)
SELECT r.control, count(*) AS leituras, sum(r.fresh) AS fresh,
       sum(r.gain) AS ganho, max(m.gap_mediana) AS gap_mediana
FROM r LEFT JOIN median m ON m.control=r.control GROUP BY r.control;

-- Primeira leitura, intervalos curtos e longos; recortes não equivalentes.
SELECT control, CASE WHEN gap IS NULL THEN 'primeira'
  WHEN gap < 600 THEN 'menor 600' WHEN gap >= 21600 THEN 'maior ou igual 21600'
  ELSE 'outros' END AS intervalo, count(*) AS leituras, sum(fresh) AS fresh
FROM reads WHERE at BETWEEN 1794782751-259200 AND 1794782751 AND weight IS NOT NULL
GROUP BY control, intervalo ORDER BY control, intervalo;

-- Mesmos alvos com intervalo de pelo menos 6 horas em ambos os controles.
WITH r AS (
  SELECT arm, control, fresh FROM reads
  WHERE at BETWEEN 1794782751-259200 AND 1794782751
    AND weight IS NOT NULL AND gap >= 21600
), comuns AS (
  SELECT arm FROM r GROUP BY arm HAVING count(DISTINCT control)=2
)
SELECT control, count(*) AS leituras, sum(fresh) AS fresh
FROM r WHERE arm IN (SELECT arm FROM comuns) GROUP BY control;

-- Pares exatos de alvo e intervalo presentes nos dois controles.
SELECT count(*) AS pares FROM (
  SELECT arm, gap FROM reads
  WHERE at BETWEEN 1794782751-259200 AND 1794782751
    AND weight IS NOT NULL AND gap IS NOT NULL
  GROUP BY arm, gap HAVING count(DISTINCT control)=2
);

-- Distribuição por alvo nas últimas 24 horas.
SELECT arm, count(*) AS leituras, sum(fresh) AS fresh
FROM reads WHERE at BETWEEN 1794782751-86400 AND 1794782751 AND weight IS NOT NULL
GROUP BY arm ORDER BY leituras DESC;

-- Estado atual e achados registrados para o alvo; não são recortes históricos equivalentes.
SELECT id, queries, finds, gain_cents, fresh FROM arm_stats
WHERE id='r:G182820FD013004';
SELECT target, count(*) AS achados, coalesce(sum(profit_cents),0) AS ganho
FROM research_finds WHERE target='r:G182820FD013004' GROUP BY target;
```
