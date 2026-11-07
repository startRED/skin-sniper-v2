# Verificação do funcionamento e da coleta — 07/11/2026

Pedido: conferir as áreas do app e remover atrasos evitáveis do aprendizado, preservando a régua de lucro,
as cotas e os prazos reais de avaliação. Os fatos de acesso à Steam continuam em `pesquisa-web.md`.

## Evidência e correções

- O painel e os endpoints de estado, vigia, oportunidades, compras, calibração e aprendizado responderam.
  A seção de aprendizado abriu no navegador sem avisos/erros de JavaScript. SQLite `quick_check`: `ok`.
  A amostra inicial tinha aproximadamente 126 MB de dados e 29 MB de memória do processo.
- A cobertura de históricos recentes caiu de 135 para 60 itens durante a inspeção. A agenda impedia a
  renovação pela interface autenticada antiga durante a pausa das actions. Agora a parcela de descoberta
  prioriza os comuns vigiados antes de vencerem; falhas enviadas gastam crédito, e uma rota autenticada
  principal indisponível não prende a fila que ainda pode usar anúncios públicos.
- O cache completo de vendas raras se perdia ao reiniciar. Agora preserva hora da busca, preço, float,
  seed e informação de adesivos por até 6 h. Aquecimento dos nomes normais vigiados em 15 s; após falha,
  mantém o ritmo normal de 2 min e as pausas/cotas do cliente. Sem renovar dados válidos só por reiniciar.
- As tentativas do CSFloat ficavam em `started` mesmo depois de terminar (657 registros antigos na
  inspeção). As novas tentativas finalizam com o resultado; as antigas não foram reclassificadas sem prova.
  A saúde exibida corresponde à resposta da chave enviada e é invalidada na troca de chave.
- A agenda recebia crédito extra por uma página que podia não sair após uma busca. Agora o crédito depende
  das tentativas de página da própria decisão, sem contar a saída concorrente nem pedidos não enviados.
- Uma compra incerta do mesmo anúncio podia ser reenviada após reinício. A pendência persistida agora
  impede a repetição daquele anúncio. Confirmação sem código permanece incerta. Uma falha no segundo
  salvamento não abandona a confirmação já pedida pela Steam.
- O placar de vendas comparava com um preço reconstruído dividindo pelo fator de correção. Agora compara
  com o preço efetivamente mostrado. O ajuste automático relativo permanece uma hipótese conservadora;
  não se afirma recuperar a previsão sem desconto quando houve teto de concorrente.

## Verificações

Testes locais cobrem os caminhos alterados, incluindo cache vencido/corrompido/futuro, reinício, adesivos,
prioridade e custo de histórico, pausa da principal com a extra disponível, página enviada com erro,
orçamento, isolamento da chave, resultado incerto de compra, erro de persistência e erro de previsão.
As suítes existentes abrangem moedas/taxas, filtros/parser, saldo, autenticação, compra/confirmação simuladas,
notificações, migração/retenção, aprendizado, deduplicação, cotas, proteção HTTP, eventos e arquivos web.

Revisões independentes separaram coleta/treino de compra/precificação. O congelamento da política após
ensaio interrompido ou inconclusivo foi mantido: a segurança prevista não autoriza uma promoção automática.

## Limites

- Sem compra real ou mensagem de teste para terceiros. Confirmações reais e a restrição da conta na Steam
  não são validadas por uma resposta simulada. Uma pendência incerta ainda exige conferência externa;
  não foi criada reconciliação manual no painel.
- Não há prova de mais lucro. As janelas de 48 h e dias 7–9 precisam transcorrer; o app não transforma
  ausência de informação em venda confirmada nem reduz a amostra de comprovação para parecer mais rápido.
- O exame de dependências web encontrou dois alertas baixos de ferramentas de compilação,
  `@sveltejs/vite-plugin-svelte` e `deepmerge`, sem correção disponível informada pelo npm. Nenhum alerta
  moderado, alto ou crítico nessa consulta. Não houve troca de dependências.
- A leitura de código e os testes não constituem prova de funcionamento perfeito de todos os serviços
  externos. Os limites de 429 documentados continuam sendo respeitados.
