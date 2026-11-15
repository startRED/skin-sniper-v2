# C — catálogo com prova, sem descarte por saldo

Medição offline: `cargo test catalog_funnel_read_only_snapshot -- --ignored --nocapture`.
Abre exclusivamente `file:E:/Documentos/ProjetosIA/SkinSniperV2/app/data/sniper.db?mode=ro`
com flags somente leitura/URI e uma transação. Não lê settings e não acessa rede.

## Snapshot única antes/depois

Instante Unix 1794783398; 57.911 vendas com multiple > 0 e 5.872 itens elegíveis da curadoria.
Saldo de 4.191 centavos e razão BRL/USD 5,5 são hipóteses diagnósticas, não valores lidos do app.
Pisos observados vazios nos dois cenários: simulação de seleção, não reprodução da agenda viva.

| Estágio | Skins | Seeds distintas por skin |
|---|---:|---:|
| Tabelas | 45 | não contado |
| Tabela com vendas | 29 | não contado |
| Tabela com prova admitida | 19 | não contado |
| Aprendizado global (inclui fora das tabelas) | 21 | 249 |
| Seleção com corte antigo de referência pelo saldo | 21 | 240 |
| Seleção sem corte pelo saldo | 21 | 240 |

O cenário antigo aplica o filtro de referência <= saldo sobre os mesmos itens carregados na transação;
com pisos vazios o antigo corte de todas as faixas caras não atua. Teste separado cobre esse corte com
faixas frescas todas acima do saldo, além de faixa vazia, barata e medida vencida.

Maior queda: 45 → 29, dezesseis skins sem vendas. Sem rede não há nova prova para admiti-las.
Nenhum limite de prova foi reduzido; nenhuma tabela ou skin foi inventada. Não houve crescimento nesta
snapshot. A referência documental 21/265 não era seleção. A medição anterior informada de 20/245 era
aprendizado global; não deve ser comparada como efeito do código. O banco continua recebendo vendas:
as diferenças temporais não são resultado desta alteração.

## Prova já aprendida que não entrou

Nove seeds admitidas em outros desgastes não aparecem na consulta selecionada:
- Glock-18 | Moonrise: 58, 66, 90, 224, 601, 837, 864.
- MP7 | Amberline: 269.
- M4A1-S | Solitude: 117.

A seleção usa `active` do nome/desgaste representante de maior giro; o aprendizado global conta a união
dos desgastes. Isso explica 249 versus 240; não é descarte por saldo. A união entre desgastes não foi
alterada nesta correção: sua política de admissão permanece igual.

## Correção e validação

Removidos o veto por referência acima do saldo e o veto por todas as faixas vistas caras. Mantidos
referência obrigatória, prova por desgaste, limite de 24 tarefas e redução de nota-base a 1/4 com piso
visto caro. Saldo continua limitando oportunidade; desconto e float não mudam.

Os dois testes de saldo falharam antes da implementação e passaram depois. Os testes do catálogo e
da prova por pattern passaram; o teste ignorado acima passou. A validação completa pertence ao processo
já iniciado pelo responsável. Não houve instalação nem medição de avisos novos por hora.
