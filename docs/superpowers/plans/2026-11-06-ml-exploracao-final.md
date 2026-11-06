# Melhorias do aprendizado com exploração — plano consolidado

Data: 06/11/2026. Estado: núcleo implementado e validado localmente; ganho de mercado e ampliações condicionais aguardam dados do piloto.
Código inspecionado: `main` em `0dcccd9`. O [plano base v0](2026-11-06-ml-exploracao.md) foi escrito antes das três revisões GPT 6.1 Terra e fica preservado para comparação. Este documento prevalece sobre as propostas daquele rascunho.

## Recomendação

Execução: contagem e limite físicos, candidata comum, proteção de ativos, persistência, deduplicação,
coortes e painel implementados. O protocolo coleta referência e alterna blocos automaticamente ao rodar
a versão nova; falha operacional interrompe o piloto. Hipóteses e comportamento efetivo estão em
`docs/projeto.md`, seção "Exploração fora dos favoritos". As ampliações do item 3.3 continuam
condicionadas à evidência; não se declara lucro superior nesta entrega.

Abrir uma vaga de teste entre os 40 alvos comuns para skins que o ranking atual deixa de fora. Antes de ativar, medir e limitar o custo físico dessas consultas e preservar o acompanhamento de oportunidades ativas. Só depois ampliar o teste para os cortes anteriores do catálogo, seeds, floats e modelos mais sofisticados.

A ideia do Erick está correta como direção: olhar apenas onde o aprendizado já recomenda limita a informação nova. O app já explora, mas principalmente dentro da seleção existente. A melhoria proposta é dar entrada a candidatas que hoje nem chegam a disputar consultas. Não há medição nesta tarefa que permita prometer mais lucro ou escolher um percentual ótimo.

## Diagnóstico confirmado no código

| O que existe | Limite relevante | Evidência |
|---|---|---|
| Exploração interna de 12,5% das escolhas de vigilância, alternando barato/seed/float | Só considera tarefas em `Queue.entries`; não é 12,5% do tráfego total | `src/hunt/scheduler.rs:29`, `:336-361` |
| Até 40 comuns, escolhidos por nota-base × fator aprendido | A skin precisa passar antes por código, piso, saldo e giro; boost limitado não garante entrada a candidata boa de nota-base baixa | `src/hunt/catalog.rs:245-276`; `scheduler.rs:234-264` |
| Descoberta com topo e cauda, históricos e crédito nominal | Não conhece todo o mercado; crédito por escolha não limita pedidos físicos, e históricos são percorridos por rank/nome | `catalog.rs:144-162`, `:773-795`, `:858-862`; `scheduler.rs:505-510` |
| Uma escolha pode abrir busca, livro, anúncios, histórico e fallback | Custo não equivale a `arm_stats.queries`; uma página pode ensinar várias tarefas | `scheduler.rs:647-882`; `detect.rs:224-239` |
| `SteamClient::sent()` e limitadores centralizados | Contador global pode misturar chamadas concorrentes e aumenta ao reservar vaga, antes do envio; não basta para atribuir nem impor cota | `src/steam/client.rs:633-634`, `:935-1029` |
| Aprendizado por proximidade e professor de compras virtuais | Proximidade não é dinheiro ganho; o resultado virtual é estimativa e pode faltar cobertura futura | `learn.rs:13-16`; `teacher.rs:264-365` |
| Troca da seleção encerra tarefas removidas | Rodízio ingênuo de candidata pode encerrar oportunidade ativa | `catalog.rs:835-849`; `detect.rs:966-976` |
| Colheita compara políticas depois da sombra | Dias pares/ímpares fixos, quase-achados + achados e qualquer diferença positiva elegem vencedor; isso não prova ganho | `harvest.rs:57-92`, `:159-181` |

Análise estática: não foram lidos dados privados de produção nem feitas consultas à Steam. Medições antigas nos desenhos anteriores continuam históricas, não são uma linha base atual.

## Etapa 1 — custo, continuidade e linha base

### 1.1 Contar o trabalho que realmente sai

Reaproveitar o cliente central e a agenda. Cada decisão precisa de identidade, versão, alvo, origem (`normal`, `exploracao_interna`, `exploracao_externa`, `descoberta`, `acompanhamento`), instante, contexto anterior à escolha e escala de proximidade aplicada. Vincular a cada decisão as tentativas de leitura efetivamente submetidas ao transporte, com endpoint, saída, resultado e custo.

- Uma requisição física custa uma vez, mesmo que cubra várias tarefas. A decisão que a provocou paga o custo; tarefas beneficiadas recebem marca de cobertura, sem multiplicar o denominador.
- Histórico disparado pela avaliação, resolução de código, novas páginas, troca extra→principal, repetição de action e fallback antigo entram na mesma conta. Reuso de cache não custa pedido.
- Falha de rede depois de iniciar uma tentativa consome orçamento operacional conservadoramente. Cancelamento, erro de construção ou bloqueio antes do envio não viram consulta observada nem resultado negativo do alvo. Não alegar que a Steam recebeu o que o cliente não pode comprovar.
- Compra, login e sessão ficam fora do orçamento de pesquisa e continuam com suas prioridades e proteções. A cota de leitura nunca impede uma compra autorizada.
- O coletor de CSFloat mantém contagem própria; sua cota não é confundida com a da Steam.

Preferir um contexto por decisão e um resumo compacto das tentativas ao desenho de uma plataforma genérica de eventos. Um delta global de `sent()` pode ajudar no diagnóstico, mas não serve como atribuição sob concorrência nem como trava.

### 1.2 Cota aplicada antes de cada tentativa

O limitador atual continua como única autorização de ritmo. A exploração substitui parte do trabalho existente; não ganha trabalhador, IP, crédito extra nem rajada próprios. Antes de cada tentativa opcional, verificar saldo de orçamento, inclusive nas repetições automáticas. Sem crédito, pausar aquele passo e retomá-lo depois.

Antes do piloto ao vivo, definir no protocolo: teto absoluto de tentativas exploratórias por janela, fração máxima do total de leituras, limite de consultas adicionais por candidata, limite do crédito acumulado e comportamento no arranque/reinício. Os valores vêm da contagem e da simulação, registrados como hipóteses. Até serem definidos e testados, o piloto fica desligado. Reiniciar não pode renovar uma cota já gasta; reserva persistida e não concluída é reconciliada conservadoramente.

Não transformar automaticamente o crédito nominal de descoberta em porcentagem física. A proposta v0 de tirar 5% da descoberta foi retirada: ainda não sabemos o custo e isso pode atrasar a atualização dos próprios candidatos.

### 1.3 Preservar observações e comparação

Reusar `observations`, `evaluations`, `ghosts` e `arm_stats`. Acrescentar somente os vínculos/metadados que faltam. Guardar IDs de anúncios únicos durante toda a comparação; a memória atual de `listings_seen` expira em 3 dias e não resolve isso sozinha. Relistagem identificada pelo mesmo item é marcada; ausência de identidade ou retorno fora da janela é incerteza, não deduplicação garantida.

Os registros novos seguem no máximo os 60 dias já usados pelo aprendizado e têm teto de linhas definido após medir bytes por linha e taxa de escrita. Manter IDs, atribuição e agregados de coortes até terminar sua maturação/análise; se a duração do ensaio não couber, compactar resultados suficientes antes de apagar o detalhe ou reduzir o ensaio. Nunca alterar retenção de compras para abrir espaço. Respeitar a meta existente de tamanho de dados.

Antes de comparar, registrar qual política está realmente ativa e impedir a eleição automática de `harvest` durante o ensaio, com restauração do estado anterior ao encerrar. Registrar mudanças existentes de saldo, escala, refit, endpoints e versão; não atribuir seus efeitos à exploração sem análise.

Critério de saída: custo e atribuição fecham nos testes; não há tentativa exploratória sem orçamento; ativos continuam conferidos; existe linha base atual coletada sem trocar o algoritmo.

## Etapa 2 — piloto mínimo fora dos favoritos

### 2.1 Uma candidata comum dentro do limite atual

Reservar uma das 40 vagas comuns para uma candidata de fora do ranking selecionado. Ela continua dentro da fila normal e usa os mesmos filtros, agrupamentos, notas, avaliação e tratamento de erros. Consultá-la avulsa por fora da fila parece menor, mas perderia contratos que hoje criam estatísticas e montam os alvos de consulta.

O primeiro conjunto contém apenas comuns já descobertos, com código, piso compatível, histórico fresco e giro exigidos pela regra atual. A fila de candidatas vem de todos os elegíveis fora do top 40, não somente da posição 41. Usar rodízio persistente, com famílias alternadas e visitas separadas no tempo, priorizando quem nunca foi testado ou está há mais tempo sem teste. Não cruzar preço × desgaste × horário em dezenas de grupos sem dados.

A candidata participa de uma parte dos slots comuns de exploração já existentes; quando só restar a vaga de acompanhamento protegido, o teste espera. A divisão desses slots é hipótese definida na simulação. “Uma das 40 vagas” não significa “1/40 das consultas”: a trava física da etapa 1 continua obrigatória. Enquanto estiver em teste, toda escolha da candidata passa por essa trava, inclusive se sua nota a colocaria no ranking normal; ela não pode escapar da cota por outro caminho da agenda.

### 2.2 Regra de troca e de promoção

- Sem vaga removível com segurança, não entra candidata nova. Tarefa com oportunidade ativa fica acompanhada; o rodízio espera. Troca de saldo e perda de elegibilidade continuam obedecendo às regras de segurança, com motivo registrado.
- Se a candidata gera oportunidade, ela segue recebendo conferências até o encerramento normal. Retirar a candidata do teste não pode, por si só, produzir `gone`.
- Conferir um ativo tem prioridade operacional e não é bloqueado pelo esgotamento da cota opcional de exploração. Essas conferências usam o ritmo e as vagas normais existentes, são identificadas como acompanhamento causado pelo piloto e entram no custo físico total da comparação, separadas do gasto opcional. Sem capacidade para garantir esse acompanhamento, não iniciar candidata nova; não gerar tráfego adicional nem abandonar o ativo para fechar uma porcentagem.
- Após visitas espaçadas, comparar oportunidades únicas aprovadas por tentativa física com a referência dos comuns na mesma janela; mostrar lucro previsto separadamente. Aproximar-se do lucro serve para diagnóstico e aprendizado atual, mas não promove sozinho.
- Amostra/janela e regra de evidência são fixadas antes do piloto. Com evidência suficiente, a candidata pode manter provisoriamente a vaga reservada independentemente de sua nota-base, evitando o bloqueio do fator limitado. Não criar promoção de favorito apenas por uma observação isolada.
- Para não bloquear a exploração para sempre, candidata retida e próxima desafiante alternam a mesma vaga em janelas predefinidas, desde que não haja ativo a proteger. Estatísticas e identidade das duas sobrevivem à saída e ao reinício. A comparação sequencial é indicativa, sujeita a mudanças do mercado.
- Se a evidência não basta, registrar inconclusivo e retestar; se piora, devolver ao rodízio com espera. Nenhuma exclusão permanente por nota ruim.

É um piloto de cobertura e operação. Ele alcança excluídas pelo ranking; ainda não testa itens desconhecidos, sem histórico, abaixo do piso hipotético ou abaixo do giro mínimo. Não comunicar cobertura irrestrita.

Critério de saída: alvos antes excluídos foram realmente consultados, a conta física fecha, notas sobrevivem ao reinício e nenhum ativo perde acompanhamento. Só então avançar para comparação de resultado.

## Etapa 3 — provar ganho e ampliar com critério

### 3.1 Resultado que pode ensinar sem enganar

Separar no relatório: crédito de proximidade; lucro previsto; estimativa de lucro virtual; dinheiro de venda real informado pelo Erick. Para comum, o professor projeta venda por mediana/livro futuro; para raro, fechamento pode vir de indício indireto. Não chamar nenhum deles de lucro realizado comprovado.

Agrupar resultados pela decisão original e data de observação/compra virtual, não pela data em que o professor acabou de rotular. O boletim atual por `labeled_at` pode continuar operacional, mas não compara políticas. Mostrar denominador, pendentes, sem rótulo, cobertura, origem da prova e quantas coortes já passaram do prazo aplicável. Comparar comuns e raros separadamente.

Nos raros, distinguir sinal indireto de fechamento, cobertura suficiente sem sinal e desconhecido. Falta de nova consulta, página parcial, pausa ou ausência de CSFloat não vira fracasso. Relistagem posterior pode invalidar o indício. Antes de esses rótulos guiarem uma decisão de política, fazer essa distinção no professor.

Manter a fórmula atual de recompensa no primeiro piloto. Medir quanto vem de livros, quase-achados e achados únicos, por modo/faixa, e se antecipa resultado aprovado/maduro. Mudança do menor preço do livro não identifica anúncio novo. Guardar a escala usada; não reaplicar uma fórmula futura ao passado e apresentar como resultado observado.

### 3.2 Comparação entre a versão atual e a exploração

Pré-definir um ensaio com blocos temporais sorteados e balanceados por horário/dia, no mesmo robô e no mesmo ritmo. A versão atual observada é a referência; não presumir que é sempre UCB1. A versão candidata acrescenta apenas o piloto, sem trocar preço, régua, refit ou fórmula junto.

A literatura de [experimentos alternados no tempo](https://arxiv.org/abs/2009.00148) trata justamente do efeito que permanece depois da troca. Aqui cache, notas, promoções e ativos deixam memória: a duração e a transição dos blocos precisam ser verificadas em simulação; a análise considera blocos, não anúncios independentes. Se a memória impedir interpretação confiável, o resultado fica operacional/descritivo e o desenho é revisado antes de alegar ganho causal.

Fixar antes de começar: duração e sorteio, regra de transição, mínimo de efeito útil, regra de amostra/parada, tratamento de falhas e tolerâncias de 429 e atraso de acompanhamento. Registrar versões, semente e atribuição. Não descartar blocos ruins depois de ver o resultado. Não eleger vencedor só porque passaram sete dias.

| Medida | Papel |
|---|---|
| Oportunidades únicas aprovadas / 1.000 tentativas físicas de leitura | Principal eficiência técnica, contando falhas e custo adicional |
| Oportunidades por hora útil e por hora corrida | Evita uma taxa bonita acompanhada de menos achados totais ou mais pausas |
| Estimativa virtual de coortes maduras, cobertura e incerteza | Confirmação econômica aproximada, separada por modo e fonte |
| Vendas reais registradas | Resultado real separado; sujeito à escolha do usuário, sem usá-lo como comparação aleatória |
| Novos alvos alcançados, retestados e retidos com evidência | Mostra se a exploração abriu informação nova |
| Tentativas/tempo de 429, idade da conferência dos ativos e histórico fresco | Limites operacionais que podem impedir adoção |

Adotar apenas se a comparação superar o ganho mínimo útil pré-definido com incerteza aceitável, coortes suficientes e nenhum limite operacional violado. Empate, intervalo amplo ou poucos achados: inconclusivo, mantém a referência. Falha de cota, encerramento indevido ou atraso excessivo de ativo: desliga o piloto e restaura a política sem apagar os dados. Um teste curto só aprova funcionamento.

Sombra serve para conferir decisões e custos previstos. Ela não revela o que teria acontecido em skins que ninguém consultou. O método de [avaliação offline de bandits](https://arxiv.org/abs/1003.5956) não justifica inventar resultados ou probabilidades para a agenda determinística. Probabilidades só serão registradas se uma fase futura realmente sortear candidatas com distribuição conhecida; ver também a [documentação oficial do Vowpal Wabbit](https://vowpalwabbit.org/docs/vowpal_wabbit/python/latest/tutorials/python_Contextual_bandits_and_Vowpal_Wabbit.html).

### 3.3 Próximas melhorias, condicionadas ao resultado

1. Dar rodízio persistente também à obtenção de históricos/códigos e descoberta dentro das cotas atuais. Só assim a exploração chega a candidatas excluídas antes do top 40. Testar cortes hipotéticos de giro/piso como pesquisa separada; a régua de publicar/comprar continua intacta.
2. Exploração externa de seed/float, com filtros e rodízio próprios, depois de resolver cursores, cobertura e custo. Não ampliar faixas cegamente: uma consulta comum não substitui consulta filtrada. Não generalizar um seed valorizado para toda a família.
3. Amostra pequena de recusados mais distantes com acompanhamento virtual etiquetado e custo próprio; excluir esses novos rótulos do refit e placar padrão até validar. Ausência de futuro continua desconhecida.
4. Se houver evidência de gargalo, testar escala/recompensa por modo e compartilhamento simples de informação entre alvos parecidos, em sombra e com validação temporal separada. Horário só entra se houver amostra por faixa; sem rede neural ou dependência nova por padrão.

Não executar tudo de uma vez. Cada ampliação precisa demonstrar uma lacuna do estágio anterior e passar pelo mesmo teto de tráfego e prova de resultado.

## Entregas pequenas e testes indispensáveis

| Entrega futura | Alcance provável | Prova de aceitação |
|---|---|---|
| Contagem/orçamento e linha base | `steam/client.rs`, chamadores em `scheduler.rs`, `catalog.rs`, `detect.rs`, `orders.rs`; `db.rs` | Cache, bloqueio antes do envio, falha de rede, 429, action repetida, extra→principal, legacy, agrupamento, concorrência e reinício fecham a conta; compra/login não dependem da cota exploratória |
| Candidata comum e continuidade | `catalog.rs`, `scheduler.rs`, estado em `hunt/mod.rs`, `detect.rs`, `watch.rs` | Nota-base baixa/zero não impede teste; rotação alcança fora top 40; várias visitas; todos vazios; ativos protegidos; saldo muda; sem vaga segura espera; stats/cursor sobrevivem |
| Coortes e rótulos confiáveis | `teacher.rs`, `ledger.rs`, `db.rs`, `harvest.rs` | Sem futuro não vira derrota; reavaliação/reaparição não duplica achado; grupos/fontes separados; retenção deixa analisar a coorte; eleição automática não atravessa o ensaio |
| Ensaio e visibilidade mínima | `/api/learning`, `/api/watch`, `src/web/learning.rs`, `web/src/lib/types.ts`, `Learning.svelte`, `Watch.svelte`, testes e contratos afetados | Estado do piloto, cota, novos alvos e resultado/inconclusivo legíveis no celular; compatibilidade dos consumidores; simulação com relógio, políticas e mercado mutável |

Simular primeiro o caso decisivo: a melhor skin está fora da seleção e a favorita antiga piora. Também testar catálogo desequilibrado, sinais esparsos, ausência de achados, modos/endereços indisponíveis e mudanças de saldo. A simulação prova limites e recuperação, não inventa rentabilidade do mercado.

Os comandos de verificação são os do `AGENTS.md` para as partes alteradas. Nesta entrega apenas documental, conferir conteúdo, links, escopo e ausência de alterações no app; não rodar tráfego Steam nem compilar o programa para validar texto. Na implementação, registrar o comportamento realizado em `docs/projeto.md` e manter fatos de mercado em suas fontes próprias.

## O que as três revisões mudaram

| Revisor GPT 6.1 Terra | Contribuição incorporada |
|---|---|
| Exploração e aprendizado (`ml_exploracao`) | Vaga interna de candidata comum; alcance além do top 40 sem prometer todos os filtros; promoção explícita independente de nota-base; proteção de ativos; adiar modos externos complexos |
| Qualidade e validação (`ml_validacao`) | Deduplicação por coorte, rótulos com cobertura/incerteza, separar dinheiro previsto/virtual/real, controlar eleição da colheita, comparar por blocos e custo físico |
| Viabilidade e escopo (`ml_viabilidade`) | Orçamento antes de cada tentativa, limite de `sent()` global, retries/fallback/reinício, contexto compacto por decisão e execução em etapas reutilizando o código atual |

Retirado do v0: 5% físico fixo retirado da descoberta; modelo/contexto e recusados distantes no primeiro piloto; promessa de contrafactual em sombra. Sem código de execução, compra automática, nova infraestrutura ou mudança da régua financeira nesta entrega.
