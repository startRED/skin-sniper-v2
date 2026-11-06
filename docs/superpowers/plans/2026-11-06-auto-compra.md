# Compra sem celular e auto-compra — plano futuro (não ratificado)

> Plano de roadmap, não de execução. Nada aqui entra antes dos portões da seção 1. Quando um portão abrir,
> transforme a fase em plano de implementação (superpowers:writing-plans) com tarefas e testes.

**Origem:** pedido do Erick em 06/11/2026, a partir de uma resposta da IA do Google sobre "bot que compra sem
pedir confirmação no celular" (SDA + `steam-user`/`steam-totp` em Node). A ideia central está certa: quem tem o
`identity_secret` da conta consegue aprovar confirmações sem o celular. O resto foi adaptado ao v2 (seção 5).

**O que o v2 já tem:** login por QR como **MobileApp** (`pesquisa-web.md` 9: refresh token renovável, token com
`aud` `["web","mobile"]`), `steam/buy.rs` com o fluxo `need_confirmation` (reenvia com o id a cada 2 s por até 60 s
enquanto o Erick confirma no app), uma compra por vez e recusa acima do saldo (`projeto.md` "Compra").

## 0. Dois recursos diferentes, com riscos diferentes

| | A. Confirmação sem celular | B. Auto-compra |
|---|---|---|
| quem decide comprar | o Erick (clique duplo no painel) | o programa |
| o que muda | o servidor aprova o `need_confirmation` sozinho | o programa chama `buylisting` sem clique |
| pior bug | aprova algo que não devia (seção 3) | gasta a banca num palpite errado |
| depende de | `identity_secret` | A + histórico de flips medido |

A vem primeiro e pode existir sem B. B nunca existe sem A, porque compra sem clique que espera o celular não é
automática.

## 1. Portões (todos, antes de qualquer código)

1. **Primeira compra pelo painel do v2 concluída** (o POST real nunca rodou no programa antigo: `conhecimento.md` 1.9).
2. **Medir se A é necessária.** Contar, nas compras reais do v2, quantas vieram com `need_confirmation`.
   O steampy #447 relata que, depois de uma confirmação, as compras seguintes na mesma sessão não pediram de novo.
   Se for raro, A vale pouco e o risco da seção 3 não se paga.
3. **Para B:** o primeiro flip manual completo no v2 (compra → 7 dias de trava → venda) e os guardrails da seção 4
   aceitos pelo Erick. Regra combinada em 31/08/2026: auto-compra só depois de flip manual validado.
4. **Decisão do Erick** sobre de onde vem o `identity_secret` (seção 2). Ninguém decide por ele.

## 2. De onde vem o `identity_secret` (decisão do Erick)

A conta tem um autenticador só. O segredo é gerado quando o autenticador é vinculado e fica no aparelho.

| opção | custo | observação |
|---|---|---|
| Mover o autenticador para o Steam Desktop Authenticator (SDA), como sugeriu o Google | **HIPÓTESE a confirmar:** trava de troca e Mercado (a comunidade cita ~15 dias) ao remover/trocar o autenticador; o app do celular deixa de ser o autenticador | SDA guarda o `.maFile` (JSON com `shared_secret`, `identity_secret`, `device_id`). Baixar só do repositório oficial no GitHub |
| Extrair do celular atual | Android exige root ou backup do app; iPhone, praticamente inviável | sem trava, mas arriscado de fazer |
| O próprio v2 vincular um autenticador novo (`ITwoFactorService/AddAuthenticator`) | a mesma trava da 1ª linha, mais código de vínculo e o código de revogação para guardar | evita programa de terceiros; só vale se SDA for descartado |
| Não ter | nenhum | continua confirmando no celular; o painel já espera 60 s |

Antes de escolher: abrir `https://help.steampowered.com/en/wizard/HelpWhyCantITrade` e a página de ajuda da Valve
sobre remoção do autenticador para medir a trava real em 2026. Com banca de R$ 18–80, 15 dias parados custam pouco;
uma conta comprometida custa tudo.

## 3. Segurança (o `identity_secret` aprova **qualquer** confirmação)

Inclusive troca mandando o inventário inteiro para outra conta e anúncio de venda a R$ 0,03. Por isso:

- **Só aprovar o que o próprio v2 acabou de pedir:** confirmação de tipo **12** (compra no Mercado) com
  `creator_id == listingid` da compra em andamento (`pesquisa-web.md` 3.2). Nunca troca, nunca anúncio de venda,
  nunca "aceitar todas". Não existe rota HTTP que aprove confirmação por id vindo de fora.
- Segredo guardado como as outras chaves (`settings.rs`, leitura mascarada), em `data/` fora do git, nunca em log,
  nunca devolvido pela API. Gravar e apagar só por loopback: o painel ouve em `0.0.0.0` para o celular.
- Ligar A recusa subir se o painel estiver sem senha/guarda de Host na LAN.
- Interruptor no painel ("aprovar compras sozinho": desligado por padrão) e kill switch que volta ao fluxo do celular.
- Log de auditoria de cada aprovação: hora, listing, valor, resultado. Sem o segredo, sem a chave derivada.

## 4. Guardrails da auto-compra (B), combinados em 31/08/2026

- Tetos em config: por item, por dia e % da banca. Recusa ao passar de qualquer um.
- Só compra oportunidade de tese com prêmio **medido** (correção automática com ≥ 5 vendas do tipo, `projeto.md`);
  achado estranho ou de confiança baixa continua só aviso.
- Reconsulta o preço exato no instante da compra; se mudou, não compra.
- Idempotência por listing ID persistida no SQLite (nenhuma compra repetida depois de reiniciar).
- Pausa obrigatória depois de cada compra e kill switch nas primeiras semanas; Telegram avisa toda auto-compra.
- Risco de ToS da Steam: avisado e aceito conscientemente pelo Erick em 31/08/2026.

## 5. Correções à receita do Google

- O v2 é Rust, não Node. Não precisa de `steam-user` nem de `steam-totp`: o login já existe (QR MobileApp) e a chave
  de confirmação é uma conta pequena (HMAC-SHA1). O pacote Node se chama `steamcommunity`, não `steam-community`.
- **HIPÓTESE (não conferida contra o código das bibliotecas):** `startConfirmationChecker` foi depreciado no
  `steamcommunity` e `createMarketBuyOrder` não existe nele. Não importa para o v2, mas mostra que a receita não foi testada.
- Senha e segredos no código-fonte, como no exemplo, violam a regra 1 do `AGENTS.md`.
- Polling de confirmações a cada 10 s é desnecessário: o v2 só consulta enquanto tem `need_confirmation` em andamento.
- "Ordem de compra" (`createbuyorder`) é outro fluxo. O v2 compra anúncio (`buylisting`); ordem de compra fica fora.

## 6. Desenho da Fase A (quando os portões abrirem)

Protocolo do app móvel (fonte: SteamDesktopAuthenticator / `node-steamcommunity`; **conferir ao vivo** com sonda
antes de codar e salvar respostas em `testdata/`):

```
time = segundos Unix (u64, big-endian 8 bytes)
key  = base64( HMAC-SHA1( base64decode(identity_secret), time_be || tag ) )   # tag "conf" para listar, "allow" para aprovar
GET  https://steamcommunity.com/mobileconf/getlist?p=<device_id>&a=<steamid>&k=<key>&t=<time>&m=react&tag=conf
GET  https://steamcommunity.com/mobileconf/ajaxop?op=allow&cid=<id>&ck=<nonce>&p=…&a=…&k=<key allow>&t=…&m=react&tag=allow
```

- Módulo novo `src/steam/confirm.rs`: `confirmation_key(secret, time, tag)` pura e testada contra vetor conhecido
  (gerar com SDA ou com a biblioteca Node uma vez, guardar em `testdata/`); `list()`; `approve(id, nonce)`.
- `buy.rs`: em `need_confirmation` com A ligada, lista, acha **uma** confirmação tipo 12 com `creator_id == listingid`,
  aprova e reenvia com `confirmation=<id>` (fluxo atual). Sem match, cai no fluxo do celular. Nada de tentar de novo
  em rajada: o "too many purchases without confirmation" pede ~5 min (`pesquisa-web.md` 3.2).
- Hora: usar o relógio da Steam (`ITwoFactorService/QueryTime`) se o do PC divergir; chave com hora errada é recusada.
- Crates: HMAC-SHA1 e base64 precisam de `hmac`, `sha1` e `base64` (regra 5: motivo escrito no PR). Alternativa sem
  crate: SHA-1 + HMAC à mão (~60 linhas) com os vetores do RFC 2202; preferir as crates, código de cripto à mão é dívida.
- **HIPÓTESES a medir na sonda:** que o cookie do login por QR MobileApp é aceito em `mobileconf`; que `device_id`
  pode ser o do `.maFile` (`android:<uuid>`); formato atual de `getlist` (`conf[].type`, `creator_id`, `nonce`).

## 7. Como saber se deu certo

- A: em N compras reais com `need_confirmation`, todas aprovadas sozinhas em < 5 s, zero aprovação fora de tipo 12,
  zero segredo em log (`grep` no `sniper.log`).
- B: flips automáticos com lucro realizado ≥ o previsto pela correção automática, nenhum teto estourado, nenhuma
  compra duplicada.
