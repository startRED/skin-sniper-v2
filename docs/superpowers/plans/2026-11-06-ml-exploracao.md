# Aprendizado com exploração fora da seleção — plano base v0

Data: 06/11/2026. Estado: proposta para revisão por três agentes GPT 6.1 Terra; nenhuma mudança no robô.
Base inspecionada: `main`, `0dcccd9`. Complementa os desenhos de aprendizado e mina de ouro; não os substitui.

Rascunho preservado após a revisão. Para decisões e ordem de execução, vale o [plano consolidado](2026-11-06-ml-exploracao-final.md), que retira propostas deste texto, incluindo os 5% fixos da descoberta.

## Pedido e resultado esperado

Reservar parte das consultas que o app já pode fazer para procurar oportunidades fora dos alvos que o aprendizado escolhe hoje. Uma skin receber pouca consulta ou não entrar entre as favoritas não deve impedir que ela ensine algo novo. O aprendizado decide onde olhar; a régua de compra, taxas, saldo, segurança e limitadores continuam valendo.

## O que já existe e o que falta

- `hunt/scheduler.rs`: UCB1, exploração determinística de 12,5% das escolhas de vigilância, rodízio de modos barato/seed/float, decaimento e notas persistidas. Explora somente as tarefas presentes em `Queue.entries`.
- `hunt/catalog.rs`: descoberta com páginas do topo e da cauda; seleção de até 40 comuns, 24 seeds e 16 floats. No comum, a nota-base multiplicada pelo aprendizado escolhe quem entra antes de a agenda poder explorar. Há crédito nominal de descoberta de 0,25 por escolha de vigilância, aproximadamente 20% na situação simples, não garantia de 20% das requisições HTTP reais.
- `hunt/learn.rs`: recompensa por proximidade do lucro e escala medida nas avaliações. Isso é um sinal auxiliar, não lucro realizado.
- `hunt/teacher.rs`, `ledger.rs`, `patterns.rs`: observações, compras virtuais com resultado atrasado, aprendizado de seeds fora das tabelas via CSFloat. Há dependência do catálogo observado e filtros prévios; não é cobertura irrestrita do mercado.
- `hunt/harvest.rs`: colheita em sombra, depois alternância diária e escolha por achados + quase-achados/consulta. Essa medida não comprova lucro e o vencedor não considera incerteza estatística.
- Uma escolha da agenda pode abrir livro, busca, histórico, página de anúncios e atender várias tarefas. `arm_stats.queries` e `hourly_activity.queries` não representam, sozinhos, o custo físico completo das decisões.
- O professor só cria compras virtuais para aprovados e quase-achados. Sem amostra de recusados mais distantes, a avaliação também herda a seleção atual.

Os números históricos dos desenhos anteriores não foram medidos novamente nesta tarefa. Não são a linha base atual.

## Etapas propostas

### 1. Medir antes de aumentar a inteligência

Registrar cada decisão com versão da política, alvo e modo, origem (produção, exploração interna, exploração externa, descoberta), motivo, horário e contexto que existia antes da decisão. Vincular os pedidos HTTP enviados, resultado/endpoint/saída e consultas adicionais à decisão que os causou. Separar proposta, envio real, sucesso, falha e tentativa sem envio. Não gravar cookies, chaves ou corpos sensíveis.

Separar contadores: consultas físicas totais de leitura, decisões, tarefas cobertas, anúncios novos únicos, achados únicos, lucro previsto, lucro virtual e lucro de venda real. Não somar esses três tipos de dinheiro. Guardar a versão da fórmula da recompensa; não juntar notas antigas e novas como se fossem equivalentes.

Proposta de armazenamento mínimo: um registro de decisão e um de pedidos vinculados, com agregação para o painel; aproveitar as tabelas existentes para resultados e observações. Retenção e tamanho máximos definidos na implementação, respeitando a meta atual de dados do projeto.

### 2. Exploração externa com cota fechada

Manter a exploração interna atual e reservar inicialmente até 5% do orçamento físico de leituras para alvos fora da seleção aprendida, retirando essa parcela do espaço de descoberta existente. Esse 5% é hipótese inicial, não medida nem acréscimo de tráfego. O limitador continua decidindo quando uma consulta pode acontecer.

Os candidatos vêm do catálogo completo já descoberto, não dos 40 comuns vencedores. Dividir o teste entre nunca consultados, fora da lista há bastante tempo e faixas pouco cobertas de preço/giro/desgaste; evitar que tudo vá para a família mais populosa. Reusar cache, busca agrupada e consultas existentes. Seeds e floats usam seus filtros próprios; página comum não prova ausência de raro.

Separar cortes de segurança de preferências da seleção. Investigar skins rejeitadas por nota/giro hipotético é diferente de publicar oportunidade sem histórico confiável. Não aumentar saldo, inventar prêmio, admitir faca/luva ou dispensar a avaliação atual. Sem evidência suficiente, a consulta aprende, mas não libera aviso de compra.

O orçamento deve considerar todo o caminho necessário para testar um candidato, inclusive histórico, resolução de nome e paginação. Sem crédito para concluir uma etapa, reagendar; nunca criar rajada ou dívida crescente. Limitar o número de candidatos em teste e proteger a atualização de oportunidades ativas.

Escolha de candidatos: comparar rodízio estratificado determinístico com sorteio dentro de estratos. O determinístico é reproduzível, mas não permite alegar avaliação contrafactual sem viés. Se houver sorteio, registrar o conjunto elegível e a probabilidade real de seleção; não inventar probabilidades para UCB1 determinístico.

### 3. Fazer o teste ensinar e ter fim

Cada candidato precisa de várias observações separadas no tempo; uma página vazia não condena uma skin. Não contar repetição do mesmo anúncio como informação nova. Prazos de retorno e amostra mínima são hipóteses a testar com simulação antes de fixar valores.

Se um alvo mostra sinal melhor e evidência suficiente, passa a disputar a seleção normal. Se não rende, volta para espera com nova oportunidade futura; nenhum banimento permanente por nota ruim. Registar quantos testes viraram alvos produtivos e por quantas requisições.

Amostra limitada de recusados fora da faixa atual deve ganhar acompanhamento virtual, identificado como teste. Ausência de dados futuros é desconhecido, não prejuízo nem fracasso. Sumiço de anúncio continua sendo indício de fechamento, não comprovante de venda. Reaproveitar `ghosts` e fontes já coletadas antes de pedir novas consultas de acompanhamento.

### 4. Aprender com resultado melhor, sem pular direto para modelo grande

Manter proximidade como sinal rápido e medir se ela antecipa oportunidades aprovadas e resultado virtual depois do prazo. Distinguir recompensa por anúncio, por consulta e por tempo desde a consulta anterior; avaliar se soma de muitos quase-achados ou um raro de preço alto domina a agenda indevidamente.

Primeiro corrigir cobertura, custo e qualidade dos resultados. Depois, se houver amostra, testar ajustes pequenos por modo/faixa de preço/giro e compartilhamento de informação entre skins semelhantes. Contexto de horário e família entra como candidato em sombra, com comparação temporal fora do período de ajuste. Não começar com rede neural, Python, dependência nova ou dezenas de variáveis.

### 5. Comparação honesta e reversão

Comparar política atual com exploração externa em blocos temporais balanceados e sorteados, mantendo ritmo, saldo observado e demais mudanças constantes quando possível. Não rodar dois robôs nem dobrar tráfego. Considerar memória da agenda, compartilhamento de cache, mudança do mercado e efeito residual entre blocos. A sombra confere decisões e custo, mas não revela o resultado dos alvos que não foram consultados.

Principal medida técnica: oportunidades únicas que passam a régua por 1.000 requisições físicas de leitura; confirmar com lucro virtual de coortes já maduras, e vendas reais quando existirem. Medir também cobertura nova, ganho de alvos promovidos, desperdício de requisições, taxa/tempo de 429 e atraso de acompanhamento de oportunidades ativas. Quase-achados não elegem vencedor sozinhos.

Teste curto valida operação. Não declarar lucro superior por sete dias corridos ou por poucos achados. Se não há dados suficientes ou o resultado empata, manter a política atual. Reversão deve ser possível sem apagar observações e sem reiniciar o histórico do app.

## Alcance provável da implementação futura

| Parte | Arquivos / contratos |
|---|---|
| Seleção de candidatos externos | `src/hunt/catalog.rs`, `scheduler.rs`, estado em `mod.rs` |
| Decisão, orçamento e atribuição de pedidos reais | `scheduler.rs`, chamadas em `catalog.rs`, `detect.rs`, `orders.rs`, `steam/client.rs` |
| Observações, deduplicação e acompanhamento | `ledger.rs`, `detect.rs`, `teacher.rs`, `patterns.rs` |
| Persistência, migração e retenção | `src/db.rs` e testes com banco temporário |
| Experimentos e política da colheita | `harvest.rs` e testes de agenda |
| Visibilidade | `/api/learning`, `/api/watch`, `src/web/learning.rs`, `web/src/lib/types.ts`, `Learning.svelte`, `Watch.svelte` e testes/fixtures afetados |
| Documentação | Atualizar `docs/projeto.md` somente quando o comportamento for implementado; números de mercado continuam em suas fontes próprias |

## Provas exigidas antes de ativar

- Simulações com relógio injetável: favorito antigo piora; skin de nota-base baixa é a melhor; sinais esparsos; catálogo desigual; todos vazios; saldo muda; três modos e endpoints indisponíveis.
- Contagem física: busca agrupada, cache, histórico adicional, falha antes de enviar, 429, queda de rede e repetição; uma requisição tem um custo mesmo quando cobre vários alvos.
- Exploração não excede a cota proposta, não passa pelo limitador, não duplica crédito/recompensa e não deixa oportunidade ativa sem conferência.
- Resultados atrasados vinculados à decisão original e à versão original; sem rotular aguardando como fracasso, sem relistagem como venda confirmada e sem usar futuro na escolha.
- Migração e retenção preservam compras e dados antigos; painel separa hipótese e resultado e continua legível no celular.
- Rodar as verificações do `AGENTS.md` para as partes efetivamente alteradas; tráfego real só numa etapa futura autorizada e dentro do ritmo existente.

## Perguntas para as três revisões

1. Exploração: qual é a menor mudança que dá acesso real a alvos excluídos e impede aprisionamento no padrão atual, sem gastar a cota em candidatos sem chance?
2. Medição: onde o resultado atual pode enganar, como corrigir custo/recompensa/resultado atrasado e qual experiência permite decidir com pouca amostra?
3. Implementação: quais limites e riscos aparecem no código real, o que deve vir primeiro, o que cortar e que testes provam o comportamento?

## Referências metodológicas para conferir na consolidação

- Vowpal Wabbit: tutorial oficial de contextual bandits; observar apenas a ação escolhida e registrar probabilidade quando há exploração aleatória.
- Li et al., An Unbiased, Data-Driven, Offline Evaluation Method of Contextual Bandit Algorithms: https://arxiv.org/abs/1003.5956.
- Bojinov et al., Design and Analysis of Switchback Experiments: https://arxiv.org/abs/2009.00148.

Essas fontes ajudam no método; não fornecem percentuais ótimos nem dados do mercado de skins.
