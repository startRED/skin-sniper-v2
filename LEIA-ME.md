# Skin Sniper

Caça no Mercado Steam (CS2) skins que dão lucro real depois das taxas e cabem no seu saldo, e deixa
comprar com um clique.

## Abrir

Dois cliques no atalho **Skin Sniper** da Área de Trabalho. Ele liga o programa sem janela e abre o
painel no navegador. Se já estiver ligado, só abre o painel.

- No PC: <http://localhost:8788>
- No celular, no mesmo Wi-Fi: `http://<IP do PC>:8788` (o IP aparece em Configurações do Windows → Rede)
- De longe, com Tailscale: `http://<nome-do-PC>.<sua-rede>.ts.net:8788` ou o IP do Tailscale com `:8788`

Não tem login nem senha: qualquer aparelho que alcance o PC abre o painel e pode comprar com o seu saldo.

Para desligar: Gerenciador de Tarefas → `skin-sniper.exe` → Finalizar tarefa.

## Primeiro uso

1. **Saldo:** clique no lápis ao lado do saldo e digite quanto você tem. Com o login da Steam ele
   lê sozinho.
2. **Configurações → Login da Steam:** "Entrar com o app da Steam" mostra um QR. No celular, abra o
   app Steam → Steam Guard → escanear QR. Sua senha não passa pelo Skin Sniper. Sem login ele caça
   do mesmo jeito; o login serve para ler o saldo e comprar.
3. **Chave do CSFloat** (opcional, melhora o preço de skin rara) e **Telegram** (avisos no celular).

## Como ler a tela

- **Preço:** o que você paga, já com taxa.
- **Venda rápida / média / alta:** por quanto vender depois dos 7 dias em que a Steam trava a skin
  comprada. Rápida vende logo; média é a mais provável; alta pode demorar.
- **Lucro:** o que sobra vendendo pelo preço médio, já tirando os 15 % da Steam.
- **Seguro:** ainda dá lucro vendendo pelo preço rápido.
- **Por dia:** o lucro dividido pelos dias em que o dinheiro fica preso (7 de trava + o tempo para vender).
  É a ordem padrão: o que faz o saldo crescer mais rápido vem primeiro.
- **Confiança · risco:** quanto de venda real existe por trás dos números e quanto o preço costuma
  oscilar em 7 dias.
- **Melhor combinação:** as skins que cabem juntas no seu saldo e rendem mais, já contando o risco.
  O aviso no celular e no PC só sai para skin que ainda dá lucro contando o risco.
- **Vigiando:** as skins que o Skin Sniper está olhando agora e até quanto compraria cada uma. As
  etiquetas dizem o que ele procura nela: **Barato** (anúncio abaixo do normal) e **Seed** (pattern raro). Ele vigia mais o que mais rendeu.
- **Perdidas nas últimas 24 h:** oportunidades que saíram da lista ficam por um dia após a saída, inclusive
  depois de fechar ou reiniciar. Cada skin mostra o motivo, sem botão de compra. “Vendido ou retirado” significa
  que a Steam não devolveu mais o anúncio; ela não informa qual dos dois aconteceu.
- **Conferido:** quando a Steam mostrou aquele anúncio pela última vez, não quando o vendedor o criou.
  Depois de 3 min sem confirmação aparece “Aguardando conferência”; isso não significa que vendeu.
- **"A Steam pediu calma":** a Steam limitou as consultas; a caça para sozinha e volta depois.

Os preços de venda são estimativas a partir das vendas reais, não promessa. O mercado pode cair
nos 7 dias de trava.

Quando vender uma skin comprada pelo painel, digite o valor em **Compras → Vendi por R$**. Depois de
5 vendas, se você costuma vender abaixo do previsto, o Skin Sniper passa a prever mais baixo do mesmo
jeito (até 20 %). Ele nunca prevê mais alto por causa disso.

## Para quem mexe no código

Ver `AGENTS.md` e `docs/projeto.md`. Para compilar e instalar a versão atual na pasta do programa:
`powershell -File scripts/instalar.ps1`.

Regressões do radar no navegador: `cd web && npm run build && npm run test:radar`.
Usa dados simulados e o registro arquivado da Moonrise; não consulta a Steam nem compra.
