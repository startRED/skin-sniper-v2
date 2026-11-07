# Radar, avisos e retenção — 07/11/2026

## Evidência antes da instalação

Leitura do banco da instalação real e das rotas locais, às 17h21 de Fortaleza (20h21 UTC).
Nenhum dado de exemplo foi inserido no banco real e nenhuma compra foi enviada.

- Saldo manual: R$ 36,06; a carteira Steam guardada estava em R$ 36,08. A diferença não explica a falta de achados.
- Última hora: 306 anúncios vistos pela primeira vez, 10 com limite de idade estimado, 6 com limite abaixo de 10 min.
- Últimos 30 min: 81 respostas boas de anúncios filtrados e 72 da interface pública; 10 históricos e 21 consultas
  de vendas CSFloat responderam. Não houve 429 nesse recorte. Houve duas falhas de rede e uma resposta de erro.
- Nesse recorte, nenhuma avaliação foi aprovada: 1.191 recusas por lucro/risco e 267 anúncios acima do saldo.
  Isso prova coleta real, mas não prova que havia oportunidade lucrativa perdida nessa janela.
- Exemplo recente: USP-S Tropical Breeze FT, anúncio `6413593868841505644`, R$ 1,97, seed 462,
  limite de idade estimado de 404 s. Tec-9 Red Quartz FT, `8822777277267233124`, R$ 1,90,
  seed 327, limite de 403 s. São observações reais, não recomendações de compra.

A Steam não fornece data de criação nessa consulta. `checked_at` significa que uma resposta recente devolveu
o anúncio, que pode ser antigo. A idade do registro é inferida somente quando uma consulta anterior da mesma
faixa cobria aquele preço; na primeira visita, fica desconhecida.

## Problemas corrigidos

1. Avisos dependiam da primeira aprovação. Se o mínimo de aviso só fosse atingido numa reavaliação, o aviso
   não saía. O navegador também descartava atualizações de um anúncio já conhecido. Agora a elegibilidade é
   conferida novamente, com envio persistido e sem repetir crédito do aprendizado.
2. O aviso esperava a avaliação da página inteira. Um histórico lento de outra skin atrasava o anúncio já
   aprovado. Agora o envio começa assim que a oportunidade é guardada, sem esperar os próximos históricos.
3. O Windows era iniciado sem conferir o resultado do processo; erros do PowerShell sumiam. Agora a aceitação
   é conferida, há prazo máximo e a falha fica no log. A reserva de envio tem prazo e dono; reiniciar permite
   retomar um envio interrompido, preservando avisos confirmados. Telegram não estava configurado nessa instalação.
4. O acompanhamento de raro usava a última consulta da tarefa inteira, que gira várias seeds. Ler outra seed
   podia adiar a conferência da oportunidade. Agora o acompanhamento mira a faixa do anúncio a cada 60 s,
   respeitando os limites Steam. Uma tentativa sem confirmação também espera, para não prender a agenda.
5. O histórico visível tinha limite de 50 perdas e um status genérico. Todas as perdas das últimas 24 h são
   retornadas, com motivo persistido e data do servidor. Consulta bloqueada não é tratada como venda.
6. A carga inicial podia sobrescrever um evento recebido durante a leitura. Os eventos agora esperam a carga
   terminar. A lista encerrada continua legível no celular, sem reduzir a opacidade do texto inteiro.
7. O balanço podia dizer “nenhum deu lucro” e, na mesma frase, mostrar lucro positivo. Agora diz que nenhum
   passou nos critérios de lucro e risco, mantendo a estimativa média separada da aprovação.

Os mínimos de R$ 1 e 10%, taxas, risco, orçamento e dados de aprendizado foram preservados.
Não há promessa de frequência de achados. Ter anúncio novo não significa ter lucro.

## Verificação

- Registro original arquivado da StatTrak Moonrise T1: compra R$ 8,44, lucro médio estimado R$ 10,75,
  lucro estimado com risco R$ 6,39, seed 706. O teste confirma aviso mesmo sem lucro na venda rápida.
  Esse registro é histórico e não foi apresentado como oferta atual.
- Testes com servidor local cobrem aviso que qualifica depois, reinício sem duplicar, reserva interrompida,
  aviso antes de um histórico posterior terminar, conferência da seed correta e recuo após falha.
- A retenção foi conferida com 65 perdas recentes, uma ativa e itens exatamente no limite e além de 24 h.
  Inverter a condição de retenção fez o teste falhar; o código correto foi restaurado.
- O teste de navegador cobre PC e celular: aviso único após atualização, status de indisponível, ausência de
  compra, recarga preservando o item, expiração após 24 h, ausência de erro e de rolagem horizontal.
- Um aviso explicitamente identificado como teste foi enviado pelo script real do Windows. PowerShell saiu
  com código 0 e o histórico WinRT continha o aviso. Isso verifica aceitação pelo sistema, não leitura pelo usuário.
- O Windows já estava configurado para não suspender automaticamente, tanto na tomada quanto na bateria.

Provas executáveis: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`,
`cd web && npm ci && npm run check && npm run build && npm run test:radar`, `cargo build --release`.
Testes reais que enviam compra ou mensagem Telegram não fazem parte da suíte automática.
