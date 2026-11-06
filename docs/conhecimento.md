# Conhecimento herdado do sniper em Go (para a reescrita em Rust)

Fonte: checkout local `E:\Documentos\ProjetosIA\Skin Sniper\skin-sniper\`, branch `main`, commit
`4f5f698` (03/11/2026). Nos caminhos abaixo, `S/` é essa raiz e `O/` é
`E:\Documentos\ProjetosIA\Skin Sniper\original\` (onde ficam os `data/` reais, fora do Git).
Formato da fonte: `arquivo:linha`. **HIPÓTESE** marca o que o projeto antigo nunca mediu nem travou
em código ou fixture. Nenhum segredo foi copiado.

Glossário: *ask* = preço pedido num anúncio; *venda paga* = transação fechada; *piso* = menor ask;
*buyer-pays* = o que o comprador paga (já inclui a taxa); *net* = o que o vendedor recebe.

---

## 1. Endpoints que funcionam em 2026

### 1.1 Interface antiga e opção de sair do beta

- `GET /market/listings/730/<nome>/render/?format=json` responde **200 `text/html`** com a página React
  inteira (~2 MB), não JSON, **sem selecionar a interface antiga**. (`S/docs/ref/steam-market-api.md:7-10`,
  `S/internal/steam/steam.go:49-52`) Atualização medida em 06/11: com o cookie público `bMarketOptOut=1`,
  o nome completo e `/render/?currency=7`, a resposta é JSON com anúncios, seed e float mesmo enquanto
  `/market/actions` devolve 429. Contrato e capturas: `pesquisa-web.md`, seção 10.
- O endpoint clássico `/market/pricehistory/` foi medido em 06/11: sem login respondeu 400; com a sessão
  do app e a opção antiga respondeu 200 com pontos em BRL, identificados por `price_prefix="R$"`.
  Contrato e limitações: `pesquisa-web.md`, seção 10. A página SSR continua como conhecimento de referência (1.6).

### 1.2 Código do item `G...` (pré-requisito de toda listagem)

```
GET https://steamcommunity.com/market/listings/730/<market_hash_name urlencoded>   (sem seguir redirect)
→ 302  Location: /market/listings/730/G180320BF063004
```
- Vem sem corpo e custa pouco. O código **nunca muda**, então o cache pode ser permanente. O projeto antigo persistia o mapa
  nome→código em `state.json` para não repetir o 302 depois de um restart. (`S/docs/ref/item-codes.md:1-13`,
  `S/internal/steam/steam.go:120-181`)
- Pegue o último segmento do `Location`, corte `?`/`#` e faça `PathUnescape`. Depois valide com `IsItemCode` =
  `G` + hex com tamanho total ímpar ≥ 5. (`S/internal/steam/steam.go:154-170`, `S/internal/steam/props.go:66-72`)
- Formato: `G` + protobuf em hex, só varints. Campo 3 é o `def_index` (arma), campo 4 é o `paint_index` (skin
  nominal) e o campo 6 vale 4 em arma comum e 3 em faca, com significado desconhecido. (`S/docs/ref/item-codes.md:15-27`,
  `S/internal/steam/props.go:76-112`)
- Códigos conferidos ao vivo em 31/08/2026: `G180420B6053004` = Glock-18 | Moonrise (4/694), `G1807209A023004`
  = AK-47 | Redline, `G180320BF063004` = Five-SeveN | Heat Treated, `G18F403209F033003` = ★ Bayonet | Doppler.
  (`S/docs/ref/item-codes.md:29-43`)
- Desde 09/10/2026 a URL por nome responde **302 → URL canônica por código** em 17/17 pedidos sem sessão
  (corpo de 160–184 B). (`S/docs/ref/steam-market-api.md:153-156`,
  `S/testdata/steam_listing_page_pricehistory_probe_2026-10.txt`)
- Gerar o código offline a partir de def+paint é possível, mas o campo 6 não é conhecido com segurança. O doc
  recomenda o 302. (`S/docs/ref/item-codes.md:51-53`)

### 1.3 Listagens: `QueryListingsForItem`

```
GET https://steamcommunity.com/market/actions?q=QueryListingsForItem&qp=<JSON url-encoded>
qp = [{"appid":730,"strItemName":"G180420B6053004",
       "filters":{"Exterior":["WearCategory2"]},
       "accessoryFilters":{},
       "propertyFilters":{"1":{"property_id":1,"int_min":"59","int_max":"59"}},
       "start":0}]
```
- Funciona **sem login e sem cookie**. (`S/docs/ref/steam-market-api.md:12-23`)
- Headers que o cliente antigo enviava: um `User-Agent` de Chrome 126 (`S/internal/httpx/httpx.go:35`),
  `Accept: application/json, text/javascript, */*` e `Accept-Language: en-US,en;q=0.9,pt-BR;q=0.8`. O
  `Cookie` era opcional (`S/internal/steam/steam.go:99-105,471-480`).
- `strItemName` **não aceita market_hash_name**: nesse caso a resposta é `total_count: 0` em silêncio.
  (`S/docs/ref/steam-market-api.md:56-57`)
- Resposta: `data.{more,start,total_count,listings[],facets[]}`. Em cada listagem: `listingid` (string), `unPrice`
  (centavos, lado do vendedor), `unFee` (centavos, taxa do comprador), `eCurrency` e `strSubtotal` (apesar do nome,
  é o **total**, por exemplo `"R$16.06"`). Também vêm `description.{market_hash_name,icon_url,icon_url_large,market_actions[].link}` e
  `asset.{id,assetid,asset_properties[],asset_accessories[]}`. O fixture real também traz `publisherFeeApp`, `publisherFeePct: 0.1`
  e `enhanced_appearances`. (`S/docs/ref/steam-market-api.md:25-52`, `S/internal/steam/steam.go:228-256`,
  `S/testdata/steam_AK_47___Redline__Field_Tested_.json`)
- Preço que o comprador paga = `(unPrice + unFee) / 100`. (`S/docs/ref/steam-market-api.md:52`)
- **Até 20 por chamada.** `count`, `num` e `size` são aceitos e ignorados. Só `start` pagina, e o cursor avança pelo número
  de itens **recebidos**, não por 20 fixo. O cliente confere `data.start == start pedido`.
  (`S/docs/ref/steam-market-api.md:60-62`, `S/internal/steam/steam.go:380-382,440-447`)
- **Um código cobre todos os desgastes** e mistura comum, StatTrak™ e Souvenir, ordenados por preço. Use o filtro
  `Exterior` e descarte as listagens cujo `market_hash_name` tem outra qualidade. (`S/docs/ref/steam-market-api.md:58-59`,
  `S/internal/steam/steam.go:402-406,458-468`)
- Contrato mínimo por listagem: id, assetid, preço > 0 e `eCurrency` > 0, sem id repetido e sem mistura de moedas. Se
  qualquer listagem falhar, a página inteira é inválida. (`S/internal/steam/steam.go:385-409`, `S/docs/ref/armadilhas.md:98-103`)
- Página 200 vazia pode ser transitória: faça um retry curto e mantenha o snapshot anterior. (`S/docs/ref/steam-market-api.md:69-70`)
- Código sem anúncio, ou item acima do teto de preço da Steam (Karambit Doppler), responde
  `{"data":{"more":false,"start":0,"total_count":0,"listings":[],"facets":[]}}`.
  (`S/testdata/steam_actions_karambit_doppler.json`, `S/docs/ref/item-codes.md:46-49`)
- Link para humano: `/market/listings/730/<código>?category_Exterior=WearCategoryN&detail=<listingid>`. O `detail` rola a página
  até o anúncio. (`S/internal/steam/steam.go:319-340`)

**Filtros do lado da Valve** (`S/docs/ref/filters.md`, `S/internal/steam/steam.go:261-286`):

| filtro | forma | observação |
|---|---|---|
| desgaste | `"filters":{"Exterior":["WearCategory0..4"]}` (FN=0, MW=1, FT=2, WW=3, BS=4) | tag inválida → 0 resultados em silêncio (`props.go:34-40`) |
| seed | `"propertyFilters":{"1":{"property_id":1,"int_min":"904","int_max":"904"}}` | inteiro **como string**, faixa inclusiva, respeitado de verdade (18/18 = 904) |
| float | `"propertyFilters":{"2":{"property_id":2,"float_min":0.15,"float_max":0.16}}` | float **como número**; `int_min` na prop 2 quebra a resposta |

- Seed `0` é válida. O código antigo precisava de uma flag explícita porque, nele, 0/0 significava "sem filtro". (`S/docs/ref/filters.md:35-37`)
- `propertyFilters` aceita faixa, não lista: uma watchlist esparsa precisa de uma chamada por seed. (`S/docs/ref/filters.md:79-84`)
- Com filtro de seed ou float, o menor preço da página é o piso **do recorte**, não da skin. Num caso deu R$ 334 em vez de R$ 16,40.
  O piso precisa vir de outra chamada. (`S/docs/ref/armadilhas.md:15-18`, `S/internal/steam/steam.go:80-86`)

### 1.3.1 Resposta nula observada em 04/11/2026

No log local autorizado, recorte 04:47–06:31 UTC: 168 falhas, todas de
`QueryListingsForItem`. Oito sem diagnóstico antigo; 160 com HTTP 200, 13 bytes e
somente a chave `data` nula (121 na hora 05 UTC, 39 na hora 06 UTC).
O log não registra item nem parâmetros; não permite agrupar por item ou medir a taxa sobre todas as consultas.
Os arquivos rotacionados .1 e .2 não existiam. Nenhuma falha de histórico nesse recorte.

**HIPÓTESE:** indisponibilidade transitória da action; não há evidência de item vazio,
parâmetro incorreto ou bloqueio de taxa. Três sondas anônimas espaçadas responderam 200 com
139379 bytes e anúncios: Moonrise FT, código G180420B6053004, sem filtro de propriedade.
As duas primeiras omitiram o sort; a terceira reproduziu também sort field=0/direction=0.
Duas sondas adicionais, também espaçadas: listagens com filtro seed 58–59 (200, 58356 bytes) e
histórico do mesmo nome (200, 270361 bytes). As cinco consultas esgotaram o limite autorizado.
Não foi capturado corpo nulo ao vivo. `testdata/steam_actions_null_contract.json` é
contrato reconstruído do diagnóstico, não captura bruta. Uma página vazia real já tem
fixture em `steam_actions_karambit_doppler.json` e continua válida.

O cliente tenta novamente uma única vez após pelo menos 5 s para o envelope exclusivo
`{"data":null}` em HTTP 200. Outras chaves mantêm o erro de formato, sem repetição.
O atraso vale para o limitador global: outras consultas também aguardam esses 5 s.
O limitador global e o breaker por endpoint continuam valendo; nunca se converte nulo
em lista vazia, para preservar dados anteriores. Formato desconhecido continua erro.

### 1.4 Moeda

- `eCurrency: 7` = BRL. As listagens do fixture real vêm em `eCurrency:7` com `strSubtotal "R$202.80"`.
  (`S/testdata/steam_AK_47___Redline__Field_Tested_.json`)
- **A moeda das listagens vem da geolocalização do IP.** Nenhum parâmetro de moeda é respeitado no `actions`. Rodar
  de uma VPS fora do Brasil muda a moeda. Valide `eCurrency` em toda página e descarte a página se divergir.
  (`S/docs/ref/steam-market-api.md:63-64,74-75`, `S/docs/ref/armadilhas.md:45-50`)
- `priceoverview` recebe `currency=7` como parâmetro (ver 1.5). `search/render` responde em **USD** mesmo com o cookie
  `steamCountry=BR`. (`S/docs/ref/steam-market-api.md:113`)
- Formatos de dinheiro diferentes: o `actions` usa `"R$16.06"` (ponto decimal) e o fixture-contrato do priceoverview usa
  `"R$ 1.234,56"`. Esse fixture é contrato escrito à mão, não corpo bruto. (`S/testdata/steam_priceoverview_brl_contract.json`)

### 1.5 `priceoverview`

```
GET https://steamcommunity.com/market/priceoverview/?appid=730&currency=7&market_hash_name=<urlencoded>
→ {"success":true,"lowest_price":"R$ …","median_price":"R$ …","volume":"1,234"}
```
- Funciona sem login. (`S/internal/steam/steam.go:514-530`, `S/docs/ref/steam-market-api.md:89-93`)
- `success:false` ou falta de `lowest_price` significa sem preço. Nesse caso a mediana **nunca** vira piso.
  (`S/internal/steam/steam.go:543-571`)
- `volume` = unidades em 24 h da **versão comum**, contadas só pelos dígitos. Ausente fica desconhecido, nunca zero.
  (`S/internal/steam/steam.go:546-560`) Esse volume não mede a procura por seed ou float raro. (`S/docs/ref/armadilhas.md:92-96`)
- 1 nome por chamada. O preço de ~146 acessórios recotado 3×/dia foi o que causou os 429 de 31/08.
  (`S/docs/ref/rate-limits.md:159-163`)

### 1.6 Histórico pago: página SSR do listing (sem login)

```
GET https://steamcommunity.com/market/listings/730/<nome>  (segue o 302 para /listings/730/<G...>)
Accept: text/html,application/xhtml+xml
```
- A página (~4 MB) embute o estado React Query com entradas `queryKey ["market","pricehistory",730,"<market_hash_name>"]`
  e `state.data = {"ecurrency":7,"prices":[{"time":<unix s>,"price_median":5.9996,"purchases":3},…]}`, além de
  `dataUpdatedAt` (ms). (`S/docs/ref/steam-market-api.md:126-146`, `S/internal/steam/pricehistory.go:11-23`)
- **Não exige login**: a série Moonrise BS começa em fev/2018, com 3.605 baldes (diários antigos, horários recentes).
  (`S/docs/ref/steam-market-api.md:141-143`) Com sessão, o resultado **não foi medido**. (`:161-162`)
- Uma página traz **todas as séries do balde**: 12 na Moonrise (5 desgastes, 5 StatTrak™ e 2 Souvenir). Série vazia
  `"prices":[]` é legítima. Grave todas, não só a pedida. (`S/docs/ref/steam-market-api.md:144-145,181-187`)
- Formato atual (10/2026): `<script id="valve-ssr-data">` com escape simples `\"`. O formato de 09/2026 usava escape
  triplo `\\\"`. O parser procura os marcadores de texto `{\"state\":{\"data\":`, `\"queryKey\":[\"market\",\"pricehistory\",730,\"`
  e `\"dataUpdatedAt\":`. (`S/internal/steam/pricehistory.go:70-107`)
- Moeda por série: valide `ecurrency` contra o `eCurrency` das listagens do mesmo payload e, se houver conflito, devolva erro.
  (`S/internal/steam/pricehistory.go:25-29`)
- **HTTP 500 com a página inteira** se reproduziu em 3/3 pedidos pela URL canônica em 09/10/2026 (`text/html`, ~3,85 MB, ~7 s).
  O corpo continha as 12 séries. Regra adotada: 500 + `text/html` → tenta o parser e, se der certo, usa as séries.
  (`S/docs/ref/steam-market-api.md:151-170`, `S/internal/steam/pricehistory_client.go:54-66`)
- Teto de leitura de 16 MiB. (`S/internal/steam/pricehistory_client.go:14`)
- O histórico mede a **skin comum do nome**. Nunca use para afirmar prêmio do exemplar raro. (`S/docs/ref/steam-market-api.md:177-178`)

### 1.7 Busca: `search/render` (catálogo e acessórios)

```
GET https://steamcommunity.com/market/search/render/?query=&start=<N>&count=10&search_descriptions=0
    &sort_column=name|price|quantity&sort_dir=asc|desc&appid=730&category_730_Type[]=<tag>&norender=1
```
- **10 resultados por chamada**. Com `count` 10, 20 ou 100 a resposta sempre traz `pagesize: 10`. Preços em **USD**: `sell_price`
  em centavos e `sell_price_text "$1.44"`. Sem login. (`S/docs/ref/steam-market-api.md:99-113`, `S/internal/steam/search.go:76-79`)
- Cada resultado traz `market_hash_name`, `asset_description.type` e `market_bucket_group_id`, **que é o código G**. Isso
  dispensa o 302 para nomes vindos da busca. (`S/docs/ref/steam-market-api.md:119`) O uso disso como fonte primária de código é
  **HIPÓTESE** (o doc diz "pode alimentar no futuro").
- Tags medidas: `tag_CSGO_Type_Pistol` (3690), `_Rifle` (2881), `_SMG` (2546), `_Shotgun` (1354), `_SniperRifle` (1302),
  `_Machinegun` (422), `tag_CSGO_Tool_Sticker` (16 104) e `tag_CSGO_Tool_Keychain` (6 904, dos quais 5 903 são Sticker Slabs que
  também aparecem no filtro de adesivo). (`S/internal/steam/search.go:16-62`, `S/docs/ref/steam-market-api.md:114`)
- `sort_column=price&sort_dir=desc` funciona. Texto com `|` em `query` devolve 0. Na cauda vem uma página curta. (`S/docs/ref/steam-market-api.md:116-118`)
- Teto de 429 deste endpoint: **não medido**. (`S/docs/ref/steam-market-api.md:121`)

### 1.8 Inventário público

```
GET https://steamcommunity.com/inventory/<SteamID64>/730/2?l=english&count=500
```
- Funciona sem login. Traz `asset_properties` (seed na prop 1, float na prop 2) e `market_bucket_group_id`.
  (`S/internal/steaminv/steaminv.go:1-11,168`)
- **Item em trade lock não vem** na lista, mas entra no `total_inventory_count`: medido 26 de 30, igual com count 75, 500 e 2000. Uma
  compra que não aparece no inventário está **travada, não vendida**. (`S/internal/steaminv/steaminv.go:12-20`)

### 1.9 `buylisting` (compra)

```
POST https://steamcommunity.com/market/buylisting/<listingid>
Content-Type: application/x-www-form-urlencoded; charset=UTF-8
Referer: https://steamcommunity.com/market/listings/730/<market_hash_name>
Origin: https://steamcommunity.com
X-Requested-With: XMLHttpRequest
User-Agent: <UA de navegador>
Cookie: <valor inteiro do header Cookie do navegador, com sessionid=… e steamLoginSecure=…>

sessionid=<sessionid>&currency=7&subtotal=<unPrice>&fee=<unFee>&total=<unPrice+unFee>
&quantity=1&billing_state=&save_my_address=0
```
(`S/internal/steam/steam.go:574-578,643-672`)
- Os campos são exatamente esses oito. **Não há campo `confirmation`** no código antigo. `subtotal` e `fee` saem
  direto de `unPrice` e `unFee` da listagem, e `currency` vem da config (7).
- Sucesso = HTTP 200 **e** JSON válido **e** `wallet_info.success == 1`. O fixture-contrato é
  `{"wallet_info":{"success":1,"wallet_balance":"1000"}}`. (`S/internal/steam/steam.go:752-773`, `S/testdata/steam_buy_success_contract.json`)
- Erros tratados: 200 com corpo vazio ou JSON inválido, 200 sem `wallet_info`, `wallet_info.success != 1`, e não-200 com
  `message` no JSON. (`S/internal/steam/steam.go:755-771`, `S/docs/ref/armadilhas.md:86-90`) **Nenhuma** resposta de erro real
  da Steam foi capturada.
- **HIPÓTESE**: esse POST **nunca foi executado contra a Steam real** pelo programa. `buy.enabled=false`, `dry_run=true` e
  "compra em um clique — ainda não validada". As compras reais (SSG seed 895, Moonrise FT a R$ 6,41) foram feitas pelo Erick
  na UI da Steam. (`S/docs/ref/steam-account.md:54-58`, `S/config.example.json:8-12`, `S/docs/CHANGELOG_DECISIONS.md:1360,1880`)
  Validar com conta secundária antes de confiar.

---

## 2. Limites de requisição

- **Teto da Steam desconhecido.** Em 31/08/2026, ~60 requisições com intervalo de 1,2–3 s passaram sem 429. Isso não mede o
  teto. (`S/docs/ref/rate-limits.md:5-6`)
- Medição ao vivo: 4 páginas de `actions` responderam 200 a cada ~3 s **enquanto** o `priceoverview` dava 429. Os endpoints
  têm saúde independente, mas o projeto tratava a cota como uma só. (`S/docs/ref/rate-limits.md:57-60`)
- Gap global padrão: `steam_gap_ms: 3000`, `price_gap_ms: 3500` (throttle extra sobre o mesmo orçamento),
  `csfloat_gap_ms: 1100` (inspect) e `csfloat.gap_ms: 1200`. (`S/config.example.json:30-32,69`) Capacidade derivada:
  3 600 000 / 3000 = 1200 req/h. (`S/docs/ref/rate-limits.md:315-316`)
- Pacing de alvos: intervalo = `ceil(gap × Σpages)`, com piso de 15 s. (`S/docs/ref/rate-limits.md:86-88`)
- **Um limitador, vários breakers**: `listings`, `priceoverview`, `ItemCode`, histórico, busca e inventário passam todos pelo
  mesmo `Limiter`. (`S/docs/ref/rate-limits.md:10-13`, `S/internal/steam/pricehistory_client.go:16-20`)
- Backoff de 429: começa em 60 s e dobra até 10 min. Uma falha dentro da pausa ativa é idempotente e não avança o degrau.
  (`S/docs/ref/rate-limits.md:26-28`, `S/internal/httpx/httpx.go:361-387`)
- Quarentena: depois de 3 janelas seguidas de 429 no `priceoverview`, ele fica em quarentena **persistente**, que sobrevive a restart e só sai
  por retomada manual. (`S/docs/ref/rate-limits.md:24-25,62-72`)
- Recusa não-429 da página de histórico (o 500): espera de 5 min por nome, dobrando até 6 h. Recusas em 3 nomes seguidos
  pausam só o histórico, de 5 min até 2 h. Isso não pausa listagens. (`S/docs/ref/rate-limits.md:30-48`)
- `search/render`: espera o reset publicado ou 60 s dobrando até 10 min, e encerra a rodada após 3 recusas.
  (`S/docs/ref/steam-market-api.md:121-124`)
- Gatilho conhecido de 429: cotar muitos nomes no `priceoverview` (~146 acessórios 3×/dia). (`S/docs/ref/rate-limits.md:161-162`)
- Duas instâncias contra a mesma cota dobram o tráfego. O projeto passou a recusar o boot com a porta ocupada.
  (`S/docs/ref/rate-limits.md:15-18`)
- Reiniciar não zera a cota remota. Medir o 429 só isoladamente, porque há risco de bloqueio temporário de IP.
  (`S/docs/ref/rate-limits.md:406-410`)
- **Sessão logada muda os limites? HIPÓTESE / não medido.** Desde 04/11 o v2 lê logado (pedido do Erick) e a 6 s, depois de horas de 429 anônimo a 4 s. O cliente antigo aceitava um cookie opcional "de conta secundária
  (read-only polling)", mas nenhuma medição comparou com e sem sessão. (`S/internal/steam/steam.go:31`,
  `S/docs/ref/steam-market-api.md:161`)
  Medido em 04/11: das 09:28 às 10:38 UTC toda leitura logada de anúncios tomou 429 (uma a cada ~10 min), e às 10:44 a mesma consulta sem login, do mesmo IP, voltou 200 e às 10:51 a logada tomou 429 de novo (mais cedo, 09:08 sem login 200 e 09:31 sem login 429, depois de várias sondas). Indício de que a Steam segura a conta, desmentido em parte às 11:06: a primeira leitura sem login do app, um minuto depois de um 429 logado e com ~5 consultas nos 5 min anteriores, também tomou 429. Hipótese que sobra: na penalidade a Steam aceita pouquíssimas consultas por janela de minutos, logado ou não. O cliente lê sem login por 6 h depois de um 429 logado (custa uma consulta a mais) e pausa até 30 min.
- CSFloat: `/listings` autenticado tem 200 respostas por janela; `/history` tem balde próprio de 500. Cabeçalhos
  `x-ratelimit-limit/remaining/reset` (unix) vêm em toda resposta. Janela provável de 1 h (**HIPÓTESE**). O inspect público
  tem fair use de ~1/s, com backoff de 15 s até 5 min. (`S/docs/ref/rate-limits.md:183-246`)
- Cache usado: código G para sempre; preço de skin por 6 h; preço positivo de acessório por 7 d; zero de acessório por 30 min;
  inventário por 10 min. (`S/docs/ref/rate-limits.md:94-98`, `S/internal/steaminv/steaminv.go:42`)

---

## 3. Identidade do item e catálogo

- Chave de identidade: o `market_hash_name` exato, com o desgaste no sufixo `" (Field-Tested)"` e prefixos `StatTrak™ `,
  `Souvenir ` e `★ `. O código G vem dele e um mesmo código vale para todas essas variantes.
  (`S/internal/steam/props.go:42-54`, `S/internal/steam/steam.go:458-468`)
- Nome de acessório: use o `market_hash_name` que a Valve manda (`Sticker | keev | Boston 2018`, `Sticker Slab | Noble (Holo)`).
  **Nunca** reconstrua com prefixo + nome. (`S/docs/ref/attributes.md:147-159`)
- **Catálogo de skins** (`internal/skindb`): vem de varrer `search/render` pelas 6 categorias de arma, sem faca. É **descoberta**,
  não economia: `sell_price` é o menor ask em USD e `sell_listings` conta asks. Uma linha vale por 24 h e o histórico guarda 8
  observações por nome. (`S/internal/skindb/skindb.go:1-39`, `S/internal/steam/search.go:19-38`) O coletor ordena por preço desc
  e faz busca binária até a faixa de preço, sem paginar do topo. (`S/internal/skindb/collect.go:38-42`)
- **Base de acessórios**: `O/data/accessorydb.json` (5,1 MB, 17 705 entradas, `price` em **USD dólares**, `source:
  steam_search_render`, `listings`). (inspeção do arquivo em 03/11/2026)
- **Base de padrões / PDB**: `O/data/patterndb.json` tem 19 skins com `seeds{seed:{label,tier,mult,listings,verdict,measured_at}}`,
  onde `mult` é o múltiplo de **ask**, não de venda. (inspeção do arquivo)
- Tabelas embutidas, copiáveis: `S/internal/patterns/data/` (ver seção 4.5).
- **Escolha de candidatos pelo orçamento B** (código):
  - faixa de descoberta = piso em **[0,5·B, 2·B]** (`S/internal/rules/discovery_band.go:5-36`). Para B = R$ 36,08,
    isso dá R$ 18,04–72,16;
  - estratos: `piso_cabe` (piso ≤ B), `piso_perto` (acima de B mas abaixo da saída sem prêmio, ~1,32·B) e `piso_longe`.
    (`S/internal/catalog/catalog.go:22-40`, `S/internal/rules/discovery_band.go:12-14`)
  - ordenação lexicográfica: viável → profundidade → vendas/dia → lucro na compra máxima → nº de anúncios → nome.
    (`S/docs/ref/targets.md:155-160`, `S/internal/catalog/catalog.go:205-222`)
  - o seletor enche por **custo em páginas**, não por quantidade de alvos. (`S/docs/ref/targets.md:136-147`)
- Faca e luva ficaram fora: 417 das 652 variantes do catálogo, nunca cabem em banca pequena. (`S/docs/ref/demanda-medida.md:42`,
  `S/internal/steam/search.go:22-23`)
- O piso sem prêmio ~1,32× vem da conta de taxa: comprando no piso, só dá para anunciar a ~1,32× e ainda fechar
  lucro R$ 0,50 / ROI 15% (ver 5.4). (`S/docs/DIAGNOSTICO-ZERO-OPORTUNIDADES.md:266-275`)

---

## 4. Atributos por listagem (float, seed, fase, adesivo, chaveiro)

### 4.1 Vêm inline. Inspect ficou dispensável

- Desde 2026 a Steam manda tudo em `asset.asset_properties`: 20/20 nos fixtures e 19/19 ao vivo. **Não precisa de inspect nem de
  CSFloat para float e seed.** (`S/docs/ref/attributes.md:1-4`, `S/docs/ref/steam-market-api.md:65-66`)

| propertyid | interno | campo JSON | uso |
|---|---|---|---|
| 1 | `#attr_set_item_texture_seed` (Pattern Template) | `int_value` (**string**) | seed 0–1000 |
| 2 | `#attr_set_item_texture_wear` (Wear Rating) | `float_value` | float 0–1 |
| 3 | `#attr_keychain_slot_seed` (Charm Template) | `int_value` | template do chaveiro |
| 4 | `#attr_sticker_slot_wear` (Sticker Scrape Level) | `float_value` | raspagem do adesivo (0 = intacto) |
| 5 | `#attr_custom_name_attr` (Name Tag) | `string_value` | — |
| 6 | `#attr_econ_action_preview` | `string_value` (hex) | payload de inspect |
| 7 | `#attr_set_item_texture_prefab` (Finish Style) | `int_value` | paint **da peça** (fase Doppler) |

(`S/docs/ref/attributes.md:96-108`, `S/internal/steam/props.go:18-30,114-141`)
- A Valve só preenche o campo do tipo da propriedade. Os outros vêm zerados e não significam nada. (`S/docs/ref/attributes.md:108`)
- **Zero conhecido ≠ ausente**: seed 0 e float 0 são valores válidos, então guarde a presença explícita (`Option` em Rust).
  Rejeite float NaN/Inf/fora de [0,1] e seed fora de [0,1000]. (`S/docs/ref/attributes.md:110-121`, `S/internal/steam/props.go:136-141,200-209`)

### 4.2 Fase Doppler

- O código do balde traz o paint genérico 415. A prop 7 traz o da peça: 415 Ruby, 416 Sapphire, 417 Black Pearl e 418–421 Phase 1–4.
  Precedência: prop 7 primeiro e, se faltar, o paint do código. (`S/docs/ref/attributes.md:123-137`, `S/internal/steam/props.go:210-222`)
  Tabela completa, com Gamma Doppler: `S/internal/patterns/data/phases.json`.

### 4.3 Adesivos e chaveiros

- Ficam em `asset.asset_accessories[]`. A distinção é por `description.type`: contém "Charm" → chaveiro, senão adesivo. Também
  conta como chaveiro o prefixo `Charm |`. O adesivo traz a raspagem em `parent_relationship_properties` (prop 4); o chaveiro traz o template em
  `standalone_properties` (prop 3). `nested_accessories` existe e sempre veio vazio.
  (`S/docs/ref/attributes.md:139-145`, `S/internal/steam/props.go:143-255`)
- Sticker Slab tem tipo de chaveiro mas nome `Sticker Slab | …`. Fixture: `S/testdata/steam_actions_mac10_sticker_slab.json`.

### 4.4 Inspect: só fallback

- `market_actions[0].link` = `steam://run/730//+csgo_econ_action_preview%20%propid:6%`. Troque o `%propid:N%` pelo
  valor da prop N; se sobrar placeholder, o link é inválido. (`S/docs/ref/attributes.md:161-165`, `S/internal/steam/props.go:257-278`)
- Se o CSFloat aceita o payload em hex puro: **não validado**. (`S/docs/ref/attributes.md:167-168`) API pública:
  `GET https://api.csfloat.com/?url=<link>`. O projeto só usava isso para enriquecer o **rank de float** (`low_rank`) quando a regra pedia.
  (`S/internal/inspect/inspect.go:35-45,113`, `S/docs/ref/attributes.md:170-180`)
- O parser HTML legado (`<div id="sticker_info">`) continua como fallback se `asset_properties` sumir.
  (`S/docs/ref/attributes.md:182-186`, `S/internal/steam/steam.go:423-429`)

### 4.5 Dados de tier de pattern (copiar destes arquivos)

| arquivo | conteúdo | fonte declarada |
|---|---|---|
| `S/internal/patterns/data/cspatterns.json` (1,6 KB) | tiers por seed de 4 skins: Deagle e Five-SeveN Heat Treated, **Glock-18 Moonrise**, Glock-18 High Beam | guias CSPatterns (Hartwich), 31/08/2026 |
| `S/internal/patterns/data/bluegem.json` (13 KB) | tiers blue gem por arma (Case Hardened) | CSMTools/blue-gems GPL-3.0 + csbluegem |
| `S/internal/patterns/data/fade.json` (330 KB) | % de fade por seed, por arma e acabamento | chescos/csgo-fade-percentage-calculator (MIT) |
| `S/internal/patterns/data/phases.json` (1,2 KB) | paint index → fase Doppler/Gamma | ByMykel/CSGO-API |
| `S/internal/patterns/data/skins.json` (230 KB) | 1 974 skins: `def_index`, `paint_indices`, `min_float`, `max_float` | ByMykel/CSGO-API |
| `O/data/patterndb.json` (214 KB) | 19 skins: label, tier e múltiplo de **ask** medido por seed | medição própria (ask, não venda) |

- **Moonrise "Center Star" T1** = 58, 59, 66, 90, 102, 224, 601, 628, 694, 706, 837, 864. T2 e T3 também estão listados. O float
  range é 0–0,65. (`S/internal/patterns/data/cspatterns.json`) A lista T1 bate com a do Hellcase, que é fonte externa e não foi medida
  pelo projeto. (`S/docs/pesquisa/achados/ranking-operacional-e-prazo.chatgpt.md:161-166`)
- Atenção às licenças: bluegem.json é GPL-3.0 e fade.json é MIT. (`_source` de cada arquivo)
- Sondar uma seed famosa não diz se a arma tem tabela: pergunte pela tabela, não por uma seed. (`S/docs/ref/armadilhas.md:115-119`)

---

## 5. Matemática de preço

### 5.1 Taxa em centavos inteiros (verificada)

```
taxa_steam = max(1, floor(s × 5 / 100))
taxa_cs2   = max(1, floor(s × 10 / 100))
comprador  = s + taxa_steam + taxa_cs2            (s = líquido do vendedor, s > 0)
net(b)     = maior s tal que comprador(s) ≤ b     (busca binária em [0, b])
```
(`S/internal/model/model.go:281-326`)
- Exemplo real: 1398 + 69 + 139 = 1606. A suíte reconcilia **111/111** pares `unPrice/unFee` reais do `actions`. Dividir ou
  multiplicar por 1,15 é proibido. (`S/docs/ref/pricing-rules.md:53-64`) Outro par real: 589 + 87 = 676 (Moonrise BS,
  `S/testdata/steam_Glock_18___Moonrise__Battle_Scarred_.json`).
- Na listagem, `unPrice` já **é** o net do vendedor e `unFee` é a soma das duas taxas. (`S/internal/steam/steam.go:239-240`)
- Conta com a fórmula acima: o exemplo do usuário, vendido a R$ 12,52, dá net R$ 10,89 (1089 + 54 + 108 = 1251 ≤ 1252, e 1090 daria 1253).
  Contra a compra de R$ 5,60, o lucro é R$ 5,29 e o ROI ~94%.
- Break-even buyer-pays = `comprador(ceil(custo))`. (`S/internal/model/model.go:338-346`)

### 5.2 P25 do histórico Steam (método e fraquezas)

- `AggregateDaily`: na janela [from, to), descarta baldes com `purchases ≤ 0` ou mediana inválida. Ordena pelas medianas, pondera por
  `purchases`, e o P25 é a primeira mediana cujo peso acumulado chega a `ceil(0,25 × total)`. O P50 é igual, no limiar de 0,5. Também calcula
  `active_days` (dias UTC distintos com compra) e `SalesPerDay = total/dias`. (`S/internal/steam/pricehistory.go:365-410`)
- Rótulo honesto: "P25 dos baldes de medianas pagas, ponderado pela quantidade". **Não** é P25 das vendas, porque a Steam só dá a
  mediana de cada balde. (`S/docs/ref/pricing-rules.md:46-48`)
- Quorum adotado: ≥ 28 compras na janela de 14 dias e ≥ 10 dias ativos. É **política herdada, não medida**. (`S/docs/ref/pricing-rules.md:42-44`)
- **Fraqueza 1 (medida)**: o P25 de um múltiplo pago num n pequeno é o **mínimo**. Com n = 2–4 vendas de peso parecido, o P25 devolve a mais
  barata, então exigir P25 ≥ 1,5× equivale a exigir que todas sejam ≥ 1,5×. (`S/docs/DIAGNOSTICO-ZERO-OPORTUNIDADES.md:207-245`)
- **Fraqueza 2 (medida)**: a coorte por seed exato, banda de fade ±0,5 pp ou tier com ≥ 2 seeds reduziu 41–70 vendas para 1–5. Houve 43
  seeds distintos em 46 vendas, e `exact_seed` só se formou em 2 de 110 alertas. (`S/docs/DIAGNOSTICO-ZERO-OPORTUNIDADES.md:149-203`)
- **Fraqueza 3 (medida)**: com retenção de 65% + taxa + lucro R$ 0,50 + ROI 15%, é preciso provar ~1,50× de prêmio pago para anunciar a só
  ~1,32× do piso. Em R$ 1,00 a exigência sobe para 2,11×. (`S/docs/DIAGNOSTICO-ZERO-OPORTUNIDADES.md:249-294`)
- **Fraqueza 4 (medida)**: usar como teto de saída o "menor ask − R$ 0,01" do recorte, num mercado denso, cola o teto no preço de compra.
  (`S/docs/DIAGNOSTICO-ZERO-OPORTUNIDADES.md:314-337`)
- Resultado: 176 sinais e 0 oportunidades, porque as 5 restrições se multiplicam. (`S/docs/DIAGNOSTICO-ZERO-OPORTUNIDADES.md:360-381`)

### 5.3 Três preços de venda: o que já existia

- `exitprice.Build` monta uma escada sobre `base = P25 Steam da versão comum` × múltiplo pago da coorte (CSFloat), com o piso no próprio
  P25 (`S/internal/exitprice/exitprice.go:288-310`):
  - **"pede alto"** = base × P75 do múltiplo, no dia 0;
  - **"provável"** = base × **mediana** do múltiplo, no dia 4. É o único degrau que pode ser somado num inventário. (`:39-61`)
  - **"última evidência"** = base × P25 do múltiplo, no dia 9;
  - **"piso"** = base × 1,0, no dia 14 (`HoldDays = 14`, `:73`).
- Sem coorte ≥ 2 vendas (`MinCohortSales`, `:70`), sem P25 Steam ou com múltiplo ≤ 0, a resposta é **"não sei"**, nunca um número. (`:262-273`)
- Prazo esperado de cada degrau = `1 / (vendas/dia × fração das vendas com múltiplo ≥ o do degrau)`, medido no CSFloat. É outro
  mercado e não prova prazo na Steam. (`:227-247`, `:118`)
- A escada começa quando a trava acaba, não hoje. Custo alto nunca eleva o degrau: o veredito passa a dizer "prejuízo provável".
  (`:280-287`, `S/docs/ref/pricing-rules.md:201-203`)
- **HIPÓTESE**: os 3 preços do v2 (alto-mas-provável / mediana / venda rápida) podem mapear para P75 / P50 / P25 dessa escada.
  Essa equivalência não foi validada.

### 5.4 Limiares e liquidez (o que estava no código)

- Kernel de compra/push: lucro ≥ **R$ 1,00** (100 centavos), ROI ≥ **20%**, prazo total ≤ **14 dias**.
  (`S/internal/rules/policy.go:13-16`, `S/docs/ref/pricing-rules.md:28-31`)
- Classificação: P25 pago ≥ 1,15×, ROI de revisão 15%, lucro de revisão R$ 0,50, retenção cross-market 0,65, `good` com ROI 20% e
  lucro R$ 1,00, scores 70/82. (`S/internal/rules/opportunity.go:17-25`)
- **Liquidez mínima da oferta rara: 0,5 venda paga comparável/dia**, decisão do dono em 10/10/2026, último commit.
  (`S/internal/rules/opportunity.go:25,235`, commit `4f5f698`)
- Escada de nível: Excelente n_ef ≥ 8 · P25 ≥ 1,50× · ROI ≥ 30% · ≥ 1/dia; Muito bom 5 / 1,30× / 25% / 0,5/dia; Bom
  3 / 1,15× / 20% / 0,3/dia. O lucro mínimo é R$ 1 nos três. (`S/docs/ref/pricing-rules.md:380-390`)
- Alerta e preço valem por no máximo **15 min** para compra. (`S/internal/engine/engine.go:195`, `S/internal/model/model.go:355`)

### 5.5 Evidência de prazo de venda (medida)

- CSFloat `/history/<nome>/sales`: lista crua de **até 40 linhas** (`limit` é ignorado), sem cursor. Cada linha traz `created_at`, `sold_at`,
  `price` (centavos USD), `item.{paint_seed,float_value,paint_index,def_index,wear_name}` e `reference.base_price`. Header
  `Authorization: <chave>`, sem `Bearer`. (`S/docs/ref/csfloat.md:78-93,15`, `S/internal/premium/demand.go:311`)
- P75 listado→vendido de 01/09/2026: Moonrise FT **13,58 h** (25,1 vendas/dia), Moonrise MW 73,8 h, SSG Acid Fade FN 18,3 h,
  USP-S Ticket to Hell MW 2,0 h e Deagle Heat Treated FT 233,8 h. (`S/docs/ref/demanda-medida.md:76-87`)
- Volume Steam comum em 14 dias: Moonrise FT 5 494 (392/dia, mediana R$ 6,63). (`S/docs/ref/demanda-medida.md:83`)
- `gone` (anúncio sumiu da página) **não é venda**: pode ter sido cancelado ou empurrado de página. (`S/docs/playbooks/medir-pattern.md:74-83`)

### 5.6 Prêmio de pattern raro: o que foi medido

- **Ask ≠ venda.** Five-SeveN Heat Treated FT, ask por seed sobre o piso R$ 16,68: seed 904 = 47×, 48 = 27×, 67 = 24,5×, seed T4 7 = 2,9×.
  É só preço pedido. (`S/docs/playbooks/medir-pattern.md:48-61`)
- Vereditos de múltiplo de ask: ≥ 3× alto, 1,6–3× médio, 1,15–1,6× baixo, < 1,15× insignificante, 1 anúncio = pouca amostra. Ausência de dado
  nunca vira prêmio zero. (`S/docs/playbooks/medir-pattern.md:37-46`)
- Vendas pagas CSFloat por tier: Moonrise MW T1 com 5 vendas e mínimo 2,62×; Moonrise FT T1 com 1 venda a 1,93×; Five-SeveN HT FT T4 chegou a 1,03×;
  P2000 Amber Fade MW T2 teve mínimo **0,84×**. **Tier visual não garante prêmio.** (`S/docs/ref/demanda-medida.md:93-111`)
- Nos alvos vivos, havia 5 a 38 vendas pagas por alvo ≥ 1,50× do base. O prêmio existe, mas é espalhado por seeds. (`S/docs/DIAGNOSTICO-ZERO-OPORTUNIDADES.md:160-171`)
- Asks Steam por seed Moonrise (snapshot de pesquisa): seed 59 com 2 anúncios desde R$ 52,29 (7,74×), seed 694 com 1 a R$ 58,61.
  (`S/docs/pesquisa/achados/glock-18-moonrise.chatgpt.md:21`)
- Flips fechados de 11/09/2026: SSG Acid Fade FN seed 872 (98,85% fade) vendido a R$ 8,24, lucro R$ 1,43 (ROI 26%), realizado 1,59× do P25 FN,
  com retenção ≈ 95% contra os 65% assumidos. Glock Moonrise FT float 0,1512 vendido a R$ 8,84, lucro R$ 1,10 (17%), 1,46×. O teto
  `min(ask × 0,95; P25 MW)` = 9,58 errou menos que a coorte CSFloat. (`S/docs/ref/demanda-medida.md:239-250`)
- Calibração gravada: P2000 Amber Fade FT, previsto 2,33×, vendido a R$ 3,25, realizado 0,96× (`O/data/calibration.jsonl`, 1 linha).
- **HIPÓTESE (só na memória de sessão, não no repo):** um flip da Moonrise seed 59 a R$ 14,39 vendido em < 2 h em 12/10, indicando que o Center Star
  tem demanda; "fade só paga no top ~5 do rank, float não paga nada; Steam ≈ 1,4× o dinheiro". O exemplo R$ 5,60 → R$ 12,52 também não
  está documentado no repo.

---

## 6. Compra

### 6.1 Fluxo antigo (`Engine.Buy`, `S/internal/engine/engine.go:3593-3865`)

1. Lock global de compra (uma de cada vez). (`:3594`)
2. Modo permite ação humana, e `buy.enabled`. (`:3596-3602`)
3. A listagem ainda está no snapshot atual e o alerta existe. (`:3603-3610`)
4. A decisão vale para a policy e a evidência atuais e foi classificada como acionável. (`:3619-3635`)
5. O preço da listagem em centavos é **igual** ao do alerta; senão aborta com "preço mudou". (`:3639-3641`)
6. Alerta e preço com ≤ 15 min. (`:3645-3651`)
7. Revalidação econômica completa com os atributos atuais. (`:3652-3773`)
8. `buy.max_price` como teto absoluto. (`:3774-3776`)
9. Trilha de auditoria gravada **antes** do POST; se ela falhar, a compra não acontece. (`:3780-3785`)
10. `dry_run` → loga e retorna sem tocar a Steam. (`:3786-3791`)
11. Reserva exclusiva do limitador; registro durável da tentativa; POST único, **sem retry**. (`:3804-3822`, `S/internal/steam/steam.go:697-711`)
12. Falha antes do transporte → tentativa encerrada como "não enviada". Falha depois → **ambígua**, a oferta fica bloqueada até a
    conta ser conferida. (`:3823-3850`, `S/internal/steam/steam.go:615-633,706-710`)
13. Sucesso → invalida o saldo conhecido e marca `bought` com o custo da tentativa. (`:3855-3864`)

### 6.2 Pré-condições externas

- Sessão: `sessionid` e `steamLoginSecure` do navegador do comprador. Quem tem o cookie entra sem senha e sem 2FA. Nunca registrar
  em log. (`S/docs/ref/steam-account.md:54-58`, `S/internal/steam/steam.go:574-578`)
- Sessão real só é aceita com o painel ouvindo em **loopback**. Na LAN, só dry-run. (`S/internal/config/config.go:911-924`,
  `S/docs/CHANGELOG_DECISIONS.md:2607-2608`)
- Mercado bloqueia sozinho se: não há compra na loja há > 1 ano, a conta é limitada (< US$ 5 gastos) ou houve estorno. O conserto é adicionar
  saldo externo e esperar 7 dias. O suporte não remove essa trava. Página de diagnóstico:
  `https://help.steampowered.com/en/wizard/HelpWhyCantITrade`. (`S/docs/ref/steam-account.md:23-52`)
- Trava de troca por mudança de nome (erro 34, ~4 h) **não afeta o Mercado**. Fonte: wiki da comunidade, indício e não certeza.
  (`S/docs/ref/steam-account.md:10-21`)

### 6.3 Trava pós-compra

- `MarketLockDays = 7`: item comprado no Mercado só pode ser revendido depois de 7 dias. A trava entra no tempo de capital, então
  `HoldDays = 7 + SellDays ≤ 14`. (`S/internal/model/model.go:348-351`, `S/docs/ref/pricing-rules.md:196-197`) O número 7 está
  como constante e confirmado indiretamente pelo inventário público, que esconde itens travados
  (`S/internal/steaminv/steaminv.go:12-20`). A duração exata não foi medida no programa: **HIPÓTESE** forte.

### 6.4 Erros vistos

- Nenhuma resposta de erro real do `buylisting` foi capturada (ver 1.9). Os casos tratados em código são 200 com corpo vazio ou não-JSON,
  sem `wallet_info`, com `success != 1`, e não-200 com `message`. (`S/internal/steam/steam.go:752-773`)
- Incidente real: um alerta bom (Moonrise FT a R$ 6,41 contra livro R$ 10,09, ROI 30%) nasceu `skipped` e o telefone não tocou.
  O Erick comprou à mão porque estava com o painel aberto. (`S/docs/CHANGELOG_DECISIONS.md:1878-1880`)

---

## 7. Telegram (validado ponta a ponta em 31/08/2026)

- Envio: `POST https://api.telegram.org/bot<TOKEN>/sendMessage`, form-urlencoded com `chat_id`, `text` (≤ 4000 caracteres, truncado com
  "…") e `disable_web_page_preview=true`. **Sem `parse_mode`**. Sucesso = 200 `{"ok":true,…}`.
  (`S/internal/notify/telegram.go:964-980`, `S/docs/ref/telegram.md:3-5`)
- 4xx com envelope `{"ok":false,"error_code":4xx}` = rejeitado sem efeito. 5xx, timeout ou corpo estranho = **efeito
  desconhecido**, não reenviar às cegas. (`S/internal/notify/telegram.go:981-1050`)
- O erro de `net/http` inclui a URL com o token: sanitize antes de logar. (`S/internal/notify/telegram.go:982-984`)
- Comandos: `GET /bot<TOKEN>/getUpdates?timeout=25&offset=<último update_id + 1>`, long-poll. O cliente HTTP precisa de timeout
  > 25 s (o antigo usava 2 min, com contexto de 60 s). Em erro, espera 10 s. Ignore chats diferentes do configurado e texto que
  não começa com `/`. (`S/internal/notify/telegram.go:1085-1123`, `S/internal/httpx/httpx.go:551`, `S/docs/ref/armadilhas.md:30-33`)
- Outbox persistida: falha de envio não bloqueia a varredura e a decisão é reconferida antes e depois do POST.
  (`S/docs/ref/telegram.md:50-58`)
- Token e chat id só no ambiente local. Criação e revogação pelo BotFather. (`S/docs/ref/telegram.md:85-86`)

---

## 8. Armadilhas que valem para a reescrita

1. `strItemName` exige o código G; com o nome devolve 0 em silêncio. (`S/docs/ref/steam-market-api.md:56`)
2. Um código mistura todos os desgastes e comum/StatTrak/Souvenir: filtre pelo `Exterior` e pelo nome. (`S/docs/ref/armadilhas.md:5-8`, `S/internal/steam/steam.go:402-406`)
3. Tag de `Exterior` inválida devolve 0 em silêncio. Nunca traduza "Field-Tested". (`S/docs/ref/armadilhas.md:10-13`)
4. Com filtro de seed ou float, o menor preço é o piso do recorte, não da skin. (`S/docs/ref/armadilhas.md:15-18`)
5. Nome de acessório não se reconstrói: use o `market_hash_name` da Valve. (`S/docs/ref/armadilhas.md:20-22`)
6. Moeda geolocalizada: valide `eCurrency` em toda página e vincule o cache à moeda. (`S/docs/ref/armadilhas.md:45-50`)
7. `search/render` vem em USD, 10 por página: nunca some com preço em BRL. (`S/docs/ref/armadilhas.md:150-157`)
8. Página 200 vazia pode ser transitória: não marque tudo como sumido. (`S/docs/ref/armadilhas.md:80-84`)
9. Página com uma listagem inválida é inválida inteira. (`S/docs/ref/armadilhas.md:98-103`)
10. Pagine por `start` += itens recebidos e confira `data.start`. (`S/internal/steam/steam.go:380-382,444-447`)
11. 429 do `priceoverview` não significa que as listagens caíram. Use breakers separados com um limitador só. (`S/docs/ref/armadilhas.md:74-78`)
12. HTTP 200 do `buylisting` não confirma compra: exija `wallet_info.success == 1`. (`S/docs/ref/armadilhas.md:86-90`)
13. Ask não é venda, e `gone` não é venda. (`S/docs/ref/armadilhas.md:68-72`, `S/docs/playbooks/medir-pattern.md:74-76`)
14. O volume do `priceoverview` mede a skin comum, não o comprador raro. (`S/docs/ref/armadilhas.md:92-96`)
15. Zero não é ausência: seed 0 e float 0 são válidos. (`S/docs/ref/attributes.md:110-121`)
16. O histórico SSR muda de formato de escape (triplo → simples em 10/2026): falhe alto se nada casar. (`S/internal/steam/pricehistory.go:70-74`)
17. O 500 da página de histórico pode trazer os dados: tente o parser antes de recusar. (`S/internal/steam/pricehistory_client.go:54-63`)
18. Long-poll do Telegram precisa de um cliente HTTP com timeout maior que 25 s. (`S/docs/ref/armadilhas.md:30-33`)
19. Config escrita pelo Bloco de Notas ou PowerShell sai com BOM: aceite ou remova. (`S/docs/ref/armadilhas.md:35-38`)
20. Arquivo com segredos (sessão Steam, bot) deve ficar com permissão restrita (0600). (`S/docs/ref/armadilhas.md:52-55`)
21. Item acima do teto de preço da Steam devolve 0, o que não é mercado parado. (`S/docs/ref/armadilhas.md:40-43`)
22. Uma lista de "kinds" de motivo copiada em 4 lugares fez o alerta aparecer no painel e sumir do Telegram: tenha uma fonte única. (`S/docs/ref/armadilhas.md:159-175`)
23. Comparar timestamps por igualdade de struct quebra entre fusos: compare o instante. (`S/docs/ref/armadilhas.md:184-201`)
24. P25 de n ≤ 4 é o mínimo; coortes estreitas demais zeram o funil. (`S/docs/DIAGNOSTICO-ZERO-OPORTUNIDADES.md:207-245`)
25. Uma segunda instância dobra o tráfego contra a mesma cota: trave uma instância por máquina. (`S/docs/ref/rate-limits.md:15-18`)
26. O "Bom" do painel foi lido como "validado" e levou a uma compra errada: separe nível de autorização na UI. (`S/docs/ref/pricing-rules.md:371-373`)

---

## 9. Fixtures reutilizáveis (não copiados; só o caminho)

Respostas reais (R) ou contratos escritos à mão (C). Os tamanhos estão em bytes. Ao lado de quase todos há um `.url.txt` ou `.url` com
a URL e o contexto da captura.

**Steam `market/actions` (`S/testdata/`)**

| arquivo | tipo | conteúdo | tamanho |
|---|---|---|---|
| `steam_AK_47___Redline__Field_Tested_.json` | R | página de listagens AK Redline, BRL, com `publisherFeePct` e `enhanced_appearances` | 204 876 |
| `steam_actions_ak_redline_stickers.json` | R | Redline com adesivos em `asset_accessories` | 191 470 |
| `steam_Glock_18___Moonrise__Battle_Scarred_.json` | R | Moonrise BS, 20 listagens, total 251 | 88 982 |
| `steam_actions_fiveseven_heat_treated_p0.json` | R | Five-SeveN HT, página 0 | 77 406 |
| `steam_actions_fiveseven_seed904.json` | R | filtro seed 904 (todas 904) | 71 234 |
| `steam_actions_fiveseven_bs.json` / `steam_Five_SeveN___Heat_Treated__Battle_Scarred_.json` | R | Five-SeveN HT BS | 63 945 / 63 941 |
| `steam_actions_bayonet_doppler.json` | R | Doppler com prop 7 = 418/420/421 (fases); a origem é requisição reconstruída | 62 339 |
| `steam_actions_mac10_sticker_slab.json` | R | Sticker Slab tipado como chaveiro | 10 668 |
| `steam_actions_charm_sample.json` | R (sanitizada, 1 linha) | listagem com chaveiro | 6 616 |
| `steam_actions_karambit_doppler.json` | R | resposta vazia (acima do teto) | 75 |
| `steam_actions_{duplicate_id,partial_malformed,short_page_40,short_page_43,sticker_missing_wear,wear_property_without_value,wear_without_seed}_contract.json` | C | casos de borda do contrato | 497–1 087 |

**Steam: outros (`S/testdata/`)**

| arquivo | tipo | conteúdo | tamanho |
|---|---|---|---|
| `steam_listing_page_pricehistory_moonrise_ft_2026-10.html` | R | página SSR atual (escape simples), 12 séries | 3 991 014 |
| `steam_listing_page_pricehistory_500_2026-10.html` | R | corpo do HTTP 500 com a página inteira | 3 851 630 |
| `steam_listing_page_react_pricehistory_moonrise_ft.html` | R | formato anterior (escape triplo), 11 séries | 4 890 315 |
| `steam_listing_page_pricehistory_probe_2026-10.txt` | R (log) | 20 pedidos: data, variante, status, bytes, ms | 1 414 |
| `steam_search_render_{stickers_p0,stickers_pricedesc_p0,stickers_kato14,charms_p0,charms_p1000,charms_tail,pistol_p1900}.json` | R | páginas de `search/render`, 10 por página, USD | 4 010–10 500 |
| `steam_priceoverview_brl_contract.json` | C | `priceoverview` em BRL | 89 |
| `steam_buy_success_contract.json` | C | `wallet_info.success:1` | 74 |
| `steam_legacy_attrs_*` | C | parser HTML legado | 10–831 |

**Outros pacotes**

| arquivo | tipo | conteúdo | tamanho |
|---|---|---|---|
| `S/internal/steaminv/testdata/inventory_730_2.json` | R (SteamID/assetid reescritos) | inventário público com `asset_properties` | 90 485 |
| `S/testdata/csfloat_listings_ak_case_hardened_ft_seed661.json` | R | CSFloat `/listings` por seed, com `reference.base_price` | 9 480 |
| `S/testdata/csfloat_listings_ak_redline_ft.json` | R | CSFloat `/listings` | 17 341 |
| `S/internal/premium/testdata/history_sales_v3_contract_sanitized.json` | R sanitizada | CSFloat `/history` com `created_at`, `float_value` | 15 057 |
| `S/internal/premium/testdata/history_sales_charm_contract_sanitized.json` | R sanitizada | `/history` de chaveiro (sem seed/float) | 11 835 |
| `S/internal/premium/testdata/history_sales_contract_sanitized.json` | C | contrato mínimo `/history` | 1 083 |
| `S/internal/inspect/testdata/csfloat_inspect_*.json` (10 arquivos) | C | respostas do inspect CSFloat (rank, vazio, erro, parcial) | 16–1 071 |
| `S/internal/notify/testdata/telegram_{get_updates,get_updates_empty,get_updates_mute,send_message_success}_contract.json` | C | Bot API | 12–336 |
| `S/testdata/f0_sent_alerts.json` | interno | alertas enviados (formato do app antigo, não da Steam) | 5 503 |

Os HTMLs de ~4 MB são o melhor teste offline do parser de histórico. Vale manter os dois dialetos.
