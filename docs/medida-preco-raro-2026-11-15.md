# Medição offline do preço raro — 15/11/2026

## Resultado D

**Não decidido** para as três guardas: `n > 3`, múltiplo enorme em skin de centavos e separação
StatTrak/Souvenir/normal. Nenhuma guarda de produção foi alterada; o fallback de desgaste permanece.
Há 8 casos pós-corte, abaixo dos 10 exigidos em D, e não é possível reconstruir integralmente os
rótulos nem a decisão da regra atual. Os 16 casos reconstruíveis ficam inconclusivos; não são
16 vendas nem prova de lucro. Não foi medida melhora de avisos confirmados ou de leitura por hora.

Meta literal: O Erick recebe mais avisos de skins que cabem no saldo e se confirmam, porque o app lê mais anúncios novos por hora, em mais skins, com preço de raro apoiado em prova, e ele vê no painel o que aconteceu com cada aviso.

## Fonte, corte e reprodução

Só SQLite, aberto por URI fixa `file:E:/Documentos/ProjetosIA/SkinSniperV2/app/data/sniper.db?mode=ro`,
com `query_only=ON` e uma transação de leitura. Sem settings, app ou rede Steam/CSFloat.
O script usa apenas a biblioteca padrão do Python e imprime JSON; não grava no banco.

```powershell
$env:PYTHONIOENCODING='utf-8'
python scripts/measure_rare_price.py
python -m unittest discover -s scripts -p test_measure_rare_price.py
```

Corte fixado antes da medição: **2026-11-13 08:26:00 UTC**, Unix **1794558360**.
Treino: aprovação `< corte`; avaliação: aprovação `>= corte`. Não houve busca de corte favorável.
Ele precede o oitavo ghost raro: 7 ghosts antes, 8 depois. A união acrescenta um caso anterior,
ficando 8/8. Nenhum limiar foi ajustado, inclusive usando os resultados posteriores.

Na execução registrada, `reads` tinha 9.326 linhas, entre 1794311920 e 1794782970.
O banco é alimentado pelo processo já existente; reexecutar garante o mesmo método, não congela
essa fonte viva. O script não inicia, para ou reinicia processo. O JSON completo permite guardar
o recibo de uma execução fora do banco; os identificadores abaixo fixam a população desta execução.

## População observável, sem alegar replay

União por `listing_id` de `ghosts WHERE approved=1 AND kind='raro'` com
`evaluations WHERE reason='aprovada' AND rare_group IS NOT NULL`. Data = primeira aprovação
nessas fontes; repetições não viram novos casos. Dados auxiliares do ghost têm precedência quando
as fontes se sobrepõem. Isso é histórico de aprovações, não todos os anúncios que a regra atual
aprovaria. Não se afirma completude de retenção.

| Fonte | Registros | Anúncios distintos |
| --- | ---: | ---: |
| Ghosts raros aprovados | 15 | 15 |
| Todas as avaliações aprovadas | 15 | 13 |
| Avaliações aprovadas com grupo raro | 10 | 8 |
| União rara | — | 16 |

Sete IDs raros estão nas duas fontes; oito só em ghosts; um só em avaliações.
Todos os 16 têm linha em observations. Os 5 registros aprovados sem grupo raro não entram
na população D. Não usar 15 avaliações como se fossem 15 avisos raros independentes.

| listing_id | Aprovação Unix | Fonte | Período |
| --- | ---: | --- | --- |
| 1712796784570457425 | 1794304829 | ghost | treino |
| 4203895644640287181 | 1794313594 | ghost | treino |
| 5456055611672804128 | 1794320876 | ghost | treino |
| 4323584935092825132 | 1794420269 | ghost | treino |
| 4809726112773751929 | 1794442768 | ghost | treino |
| 8561943119977975402 | 1794549037 | avaliação | treino |
| 7879832892286896013 | 1794549094 | ambas | treino |
| 6659742924551454401 | 1794557728 | ghost | treino |
| 2236410982722897117 | 1794558419 | ghost | pós-corte |
| 864855775413109701 | 1794567990 | ambas | pós-corte |
| 497240074063122389 | 1794568290 | ghost | pós-corte |
| 1110882788837215127 | 1794569933 | ambas | pós-corte |
| 98668873419638827 | 1794570473 | ambas | pós-corte |
| 8493351350422908213 | 1794574449 | ambas | pós-corte |
| 3445468645471575956 | 1794591879 | ambas | pós-corte |
| 6965302775625750730 | 1794693265 | ambas | pós-corte |

## Rótulos, na ordem D

1. **inconclusivo**: faixa não relida em 48 h, ou sumiço como mais barato junto com anúncio
   do grupo abaixo do previsto. Aqui a releitura da faixa não é demonstrável; ausência de prova
   não significa que nenhuma consulta ocorreu.
2. **provável falso**: permaneceu anunciado após 48 h pelo mesmo preço ou menor, ou surgiu
   anúncio do grupo abaixo do previsto, depois de vencer a primeira condição.
3. **provável achado**: mais barato da faixa continuamente até sumir, em até 48 h, sem retorno.
   Sumiço continua sendo compra ou retirada. Casos sem encaixe também são inconclusivos.

| Período | inconclusivo | provável falso | provável achado |
| --- | ---: | ---: | ---: |
| Treino | 8 | 0 | 0 |
| Pós-corte | 8 | 0 | 0 |

Zeros significam nenhum caso demonstrável nessas classes, não ausência real de falsos/achados.
Na última data de reads, um caso pós-corte nem tinha completado 48 h (P2000, ID
6965302775625750730); os outros sete tinham tempo transcorrido, mas não prova completa da faixa.

## Dados impossíveis e proteção contra informação futura

- `observations.cheapest` só guarda a posição na primeira leitura. `last_seen` é sobrescrito;
  não existe sequência de posições e preços por anúncio. Não prova continuidade como mais barato,
  nem o preço exato no instante de 48 h. `gone_at` e `relisted` também não são uma trilha de eventos.
- `reads.arm` identifica braço, não faixa. Não se pode juntar sua data a `observations.query` para
  inventar cobertura. Uma observação posterior não reconstrói páginas intermediárias e seus limites.
- `csfloat_sales.sold_at` é data da venda, não de quando o app conheceu a venda. Não há data de
  ingestão nessa tabela. Filtrar vendas por `sold_at < anúncio` sozinho permitiria informação que
  só chegou depois. Por isso não se apresenta esse filtro como contagem histórica `n` válida.
- Replay atual exigiria entradas e estado disponíveis antes de cada anúncio: vendas conhecidas,
  cache, preços-base, configuração da época e prova dos sumiços usados como referência. As tabelas
  consultadas não preservam tudo isso. Não se leu settings para tentar preencher essas lacunas.
- Preço e base do ghost são os valores gravados da decisão, não série temporal anterior ao anúncio.
  Nenhuma variável posterior foi usada como feature de guarda. Datas de aprovação só separam a
  amostra; observations só confere presença, sem orientar previsão ou decisão.
- `fast_closed` do professor usa outro critério e não substitui rótulo D. Não é validação de venda
  daquele anúncio. Os rótulos finais de 16/11 e 20/11 ainda não foram observados nesta medição.

## Guardas candidatas

| Guarda | Decisão | Impedimento |
| --- | --- | --- |
| Mais de 3 vendas do grupo | não decidido | `n` conhecido na época não reconstruível; 8 pós-corte; rótulos inconclusivos |
| Múltiplo enorme em skin de centavos | não decidido | sem limiar defensável ajustável só no treino; mesma insuficiência temporal e de rótulos |
| Separar variantes | não decidido | falta replay com evidência temporal e comparação rotulada; 8 pós-corte |

Não foram medidos números de falsos/achados removidos por guarda. Não colocar zero nesses campos:
seria confundir falta de medição com resultado. Separar nomes por prefixo hoje não mede o efeito
da separação na evidência disponível à época. O fallback de desgaste vizinho não foi alterado.

## Verificação

Cinco testes offline passaram: deduplicação e primeira data, prioridade do inconclusivo sem
cobertura, conflito de sinais, ordem falso/achado/sem encaixe e medição integrada em banco em memória
sem tabela settings (incluindo exclusão de comum e fronteira exata do corte). O script real também
foi executado na URI autorizada e produziu as contagens acima. Guardas e desempenho de produção
não foram validados por esta tarefa; nenhuma mudança de produção foi feita.
