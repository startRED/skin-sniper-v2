# Patterns sem venda guardada — 15/11/2026

Objetivo: O Erick passa a ter mais skins de pattern com prova de preço no catálogo de caça, e o app passa a gravar o que falta para provar e cortar as releituras que não rendem.

## Medição

Leitura offline em uma transação SQLite, exclusivamente pela URI
`file:E:/Documentos/ProjetosIA/SkinSniperV2/app/data/sniper.db?mode=ro`.
Nenhum valor de `settings` foi lido. Sem tráfego para Steam/CSFloat, sem alterar o banco vivo ou o app.
Tabela de patterns obtida das mesmas cinco fontes de `pricing::groups::table_skins`;
vendas agrupadas por `skin_base` (remove desgaste e prefixo StatTrak/Souvenir).

Instante Unix 1794787835: 45 skins nas tabelas, 29 com venda guardada e 16 sem venda. Os números são cobertura, não preços nem estimativa de mercado.

## Skin por skin

“Nomes elegíveis” conta os nomes com `catalog_references` que `load_items` entregava ao coletor.
Todas têm zero vendas e zero entradas no cache, inclusive após remover prefixo/desgaste.
Nenhuma consulta atribuída a esses nomes foi encontrada nas decisões ligadas a tentativas CSFloat.
A ausência desses registros não prova que nunca houve uma consulta: o coletor antigo não atribuía nome
às tentativas e o histórico tem retenção. Por isso a classificação histórica é **desconhecido** em todas,
e não “nunca pedida” ou “CSFloat devolveu vazio”. Zero vendas não prova resposta vazia.

| Skin | Nomes em items | Nomes elegíveis antes | Classificação | Por que não cobriu / ação |
|---|---:|---:|---|---|
| AK-47 \| Aphrodite | 10 | 6 | Desconhecido | Há nomes elegíveis, mas nenhuma resposta persistida; causa histórica não demonstrável. |
| AK-47 \| Case Hardened | 15 | 10 | Desconhecido | Há nomes elegíveis, mas nenhuma resposta persistida; causa histórica não demonstrável. |
| AWP \| Electric Hive | 10 | 4 | Desconhecido | Há nomes elegíveis, mas nenhuma resposta persistida; causa histórica não demonstrável. |
| AWP \| Fade | 3 | 0 | Desconhecido | Excluída integralmente pelo filtro da curadoria; corrigido o alcance da coleta. |
| AWP \| Sun in Leo | 10 | 5 | Desconhecido | Há nomes elegíveis, mas nenhuma resposta persistida; causa histórica não demonstrável. |
| Five-SeveN \| Berries And Cherries | 2 | 1 | Desconhecido | Há nomes elegíveis, mas nenhuma resposta persistida; causa histórica não demonstrável. |
| Five-SeveN \| Case Hardened | 12 | 11 | Desconhecido | Há nomes elegíveis, mas nenhuma resposta persistida; causa histórica não demonstrável. |
| Galil AR \| Phoenix Blacklight | 6 | 2 | Desconhecido | Há nomes elegíveis, mas nenhuma resposta persistida; causa histórica não demonstrável. |
| Glock-18 \| Fade | 1 | 0 | Desconhecido | Excluída integralmente pelo filtro da curadoria; corrigido o alcance da coleta. |
| M4A1-S \| Fade | 4 | 2 | Desconhecido | Há nomes elegíveis, mas nenhuma resposta persistida; causa histórica não demonstrável. |
| MAC-10 \| Amber Fade | 8 | 2 | Desconhecido | Há nomes elegíveis, mas nenhuma resposta persistida; causa histórica não demonstrável. |
| MAC-10 \| Case Hardened | 10 | 10 | Desconhecido | Há nomes elegíveis, mas nenhuma resposta persistida; causa histórica não demonstrável. |
| MAC-10 \| Fade | 3 | 1 | Desconhecido | Há nomes elegíveis, mas nenhuma resposta persistida; causa histórica não demonstrável. |
| P250 \| Plum Netting | 9 | 0 | Desconhecido | Excluída integralmente pelo filtro da curadoria; corrigido o alcance da coleta. |
| R8 Revolver \| Fade | 8 | 5 | Desconhecido | Há nomes elegíveis, mas nenhuma resposta persistida; causa histórica não demonstrável. |
| UMP-45 \| Fade | 3 | 1 | Desconhecido | Há nomes elegíveis, mas nenhuma resposta persistida; causa histórica não demonstrável. |

Não há evidência de venda guardada sob outro nome nas transformações reconhecidas pelo código.
Não se inventaram aliases. Outros nomes incorretos não podem ser descartados sem uma resposta identificável.
Nos 13 casos já elegíveis, a escolha prioriza aquecimento dos vigiados e depois giro/nome; reiniciar perde
`learn_seen` e a retenção apaga cache antigo. Esses mecanismos podem atrasar a cobertura, mas o retrato não prova que causaram cada ausência;
não foram alterados por hipótese.

## Correção mínima

- A coleta de nomes de tabela passa a enxergar também os itens já guardados fora da curadoria.
  AWP Fade, Glock-18 Fade e P250 Plum Netting deixam de ser excluídas integralmente.
  Também alcança desgastes antes excluídos das demais skins de tabela: 126 nomes adicionais no retrato, somando todas as tabelas.
- Descoberta geral (`candidates`) e seleção para a caça continuam restritas à leitura anterior.
  Não foram gerados nomes, preços, códigos nem vendas. Um nome coletável não ganha admissão à caça.
- A consulta do coletor recebe decisão com o nome exato e política `pattern_sales` no registro existente.
  Uma tentativa enviada passa a poder ser ligada ao alvo e seu resultado de transporte; resposta vazia
  continua persistida como `[]` no cache. `ok` sozinho não prova resposta com venda, e cache é a última resposta.
- Mantidos GAP, WARM_GAP, REFETCH, limitador/cota do cliente e todos os limites de prova.
  Sem migração, dependência ou parser novo. Os nomes adicionais podem preencher vagas antes ociosas e aumentar o total de consultas diárias, sempre sob o ritmo e a cota existentes. Nenhuma consulta real foi feita nesta tarefa.

As 16 skins da tabela continuam fora por falta de venda/prova. As três bases antes excluídas também continuam sem referência na curadoria; coletar vendas não remove essa exigência de admissão. Não houve crescimento medido do catálogo:
a correção só será exercida no ritmo do app quando esta versão for usada. Não se instalou nem reiniciou nada.
A obtenção de vendas futuras e sua suficiência para provar prêmio permanecem não verificadas.
Cortar releituras sem rendimento pertence à conversa da agenda; não se alteraram seus arquivos.

## Validação

O teste de alcance reproduziu o defeito: retornou 1 nome em vez de 4, sem rede.
- `cargo fmt --check`: passou.
- `cargo clippy --all-targets -- -D warnings`: passou, sem avisos.
- `cargo test`: 366 passaram, zero falhas, 11 ignorados (incluem medições manuais).
- `npm ci`: passou; informou duas vulnerabilidades de baixa gravidade nas dependências existentes.
- `npm run check`: zero erros e avisos.
- `npm run build`: passou; aviso existente sobre importações sem extensão para uma configuração futura do Vite.
- `node scripts/build-catalog.mjs --self-test` e `node scripts/collect-market-sales.mjs --self-test`: passaram.

Os dois testes novos cobrem alcance fora da curadoria sem alterar `load_items`, e atribuição da tentativa
CSFloat ao nome exato em banco temporário. Não há parser novo nem mudança de fixture.
Os avisos de dependências e configuração do painel ficaram fora desta tarefa.

