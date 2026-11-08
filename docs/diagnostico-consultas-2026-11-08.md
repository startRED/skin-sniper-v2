# Prioridade de consultas — 08/11/2026

Escopo: distribuição e clareza das consultas. Sem alteração do catálogo, seleção, preços, saldo ou critérios
de aprovação. O outro trabalho de catálogo permanece separado.

## Evidência e causa

Na instalação real, no intervalo Unix `[1794113989, 1794114589)`, foram enviadas 195 consultas Steam:
107 de livro, 29 de busca e 59 de anúncios públicos. As seis consultas CSFloat do período não entram nessa
contagem. Quatro IDs foram vistos pela primeira vez; três tinham limite de idade estimado abaixo de 10 min.
As contagens vêm de `research_attempts` e `observations`, sem emitir consultas extras à Steam.

`prefer_public_listings` exigia que a fila nativa não estivesse pausada para preferir o render da principal.
Com as actions em 429 e o render disponível, a preferência dependia de a saída extra estar livre naquele
instante. Sem essa coincidência, a caça consumia a fila pública em livro/busca antes de ler anúncios.

Agora pausa ou breaker nativo também dão preferência direta ao render, desde que a própria fila pública
esteja saudável. O espaçamento, as pausas, a proteção por endpoint, o login e o orçamento foram preservados.
O livro periódico usado nos pedidos e no aprendizado continua existindo.

## Prova

- O teste de pausa nativa com render disponível falhou antes da correção: uma leitura extra de livro.
  Depois passa e prova duas páginas públicas sem repetir livro nem actions bloqueadas, mesmo com piso alto.
- Há teste direto de pausa/breaker nativo e das proteções de pausa/breaker público, sem depender de proxy.
- O balanço distingue páginas, IDs distintos, novos no monitoramento e avaliações; duas páginas repetidas
  não duplicam IDs distintos ou novos. O ciclo seguinte zera os contadores.
- O painel informa quando os filtros de seed/float estão pausados; anúncio público não é apresentado como
  substituto de uma busca filtrada. Novo no monitoramento não é sinônimo de recém-criado na Steam.

Esta correção aumenta a parcela útil das consultas durante a pausa nativa; não garante anúncios novos ou
lucrativos em quantidade fixa. A medição após instalar fica no diretório de backup da instalação.
