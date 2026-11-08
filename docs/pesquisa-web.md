# Pesquisa web — sniper CS2 do Mercado Steam (v2)

Data da pesquisa: 2026-11-03. Fontes com URL e data (publicação ou acesso). "Sonda local" = requisição anônima feita por mim, do IP do Erick, em 2026-11-03 ~23h BRT, com `curl` (sem cookie de login). Fonte primária = resposta do próprio Steam/CSFloat ou código JS servido pelo Steam. **HIPÓTESE** = nenhuma fonte primária/credível confirmou.

---

## 0. Resumo do que muda o desenho

1. **O mercado novo (beta desde 12–13/05/2026) entrega float e seed por anúncio, sem serviço de inspect e sem login.** A ação `QueryListingsForItem` devolve `asset.asset_properties` com `propertyid 1 = Pattern Template` (seed), `2 = Wear Rating` (float), `6 = Item Certificate` (hex do inspect). Também aceita **filtro no servidor por faixa de seed e de float** e ordenação por propriedade. (Sonda local; seção 1.)
2. **`/market/listings/730/<nome>/render` sem optar pela interface antiga** responde `302` para a página nova por código G (`/market/listings/730/G180420B6053004?...`). Atualização de 06/11: a opção oficial de sair do beta recupera o JSON; medição e contrato na seção 10.
3. **Compra:** `POST /market/buylisting/<listingid>` continua, agora com `confirmation`, `tradefee_tax` e campos de cobrança. Desde ~09/07/2025 o Steam pode exigir confirmação (`need_confirmation` → confirmar no app → reenviar com `confirmation=<id>`). Clique único só funciona se o Erick confirmar no celular ou se o app tiver o `identity_secret` (seção 3).
4. **Item comprado no Mercado fica 7 dias sem poder ser revendido nem trocado** (`market_marketable_restriction: 7`, `market_tradable_restriction: 7` na própria descrição do anúncio). Com R$36, isso trava o capital por uma semana a cada flip (seção 3.4).
5. **O prêmio de padrão raro em Moonrise é pequeno de verdade.** Nas vendas reais do CSFloat (29/10 a 03/11/2026), seeds T1 da Moonrise FT saíram por US$1,05–1,50, contra preço-base de US$0,73 (cerca de 1,4–2x). Blogs falam em "a partir de US$100" (seção 5).

---

## 1. Dados de anúncio: float, seed, adesivos (2025–2026)

### 1.1 O endpoint antigo `/render`
- Sonda local (2026-11-03): `GET /market/listings/730/Glock-18%20%7C%20Moonrise%20%28Field-Tested%29/render/?start=0&count=2&currency=7&format=json` → **HTTP 302** `Location: /market/listings/730/G180420B6053004?start=0&count=2&currency=7&format=json` (HTML da página nova, sem JSON). Na prática a URL antiga só serve agora para descobrir o **código G** do grupo do item.
- Contraponto: steamy-py issue #22 (aberta 2026-10-03) ainda descreve `/render/?format=json` com `listinginfo{}`/`assets{}` como o caminho certo — https://github.com/AuthFailed/steamy-py/issues/22 e PR #30 https://github.com/AuthFailed/steamy-py/pull/30 . Não há evidência de que tenham testado contra o mercado novo. **HIPÓTESE:** o `/render` ainda funciona para quem sai do beta ("exit beta") ou para outros apps.
- Sem `item_nameid` no JSON antigo; precisava raspar `Market_LoadOrderSpread(...)` do HTML — https://www.github.com/Gobot1234/steam.py/pull/305 (histórico). No mercado novo o livro de ofertas sai por nome (1.4), sem `item_nameid`.

### 1.2 Mercado novo (beta, 12–13/05/2026)
- Notícia: PC Gamer, 2026-05-12 — https://www.pcgamer.com/games/valve-made-more-than-27-million-unique-images-for-counter-strike-listings-as-part-of-its-new-major-update-to-steams-community-market/ ; Neowin — https://www.neowin.net/news/valve-shows-off-steam-community-market-overhaul-wants-your-feedback/ ; TechPowerUp — https://www.techpowerup.com/348989/steam-community-market-gets-bigger-listings-search-and-filtering
- HypeSkins (2026-05-13): cada anúncio mostra o float exato e o pattern template, com filtros por faixa de float e de pattern — https://hypeskins.com/blog/steam-community-market-redesign-2026-guide (secundária, mas confirmada pela sonda abaixo).
- Página é SSR/React; os bundles ficam em `https://cdn.fastly.steamstatic.com/steamcommunity/public/ssr/*.js` (sonda local).

### 1.3 Protocolo das "actions" (fonte primária: JS do Steam + sonda local)
Implementação encontrada no bundle (`FetchQueryAction` / `CallMutationAction`):
- **Consulta:** `GET https://steamcommunity.com/market/actions?q=<Método>&qp=<JSON array de argumentos>` com header **`x-valve-request-type: queryAction`**. Resposta `{"data": ..., "metrics": ...}`.
- **Mutação:** `POST <rota>` com body JSON `{"m":"<Método>","mp":[...]}`, header `x-valve-request-type: mutationAction`, `Content-Type: application/json; charset=utf-8`.
- Outros headers do Steam vistos no bundle: `x-valve-action-type`, `x-valve-loader-index`, `x-valve-refetch-payload`.
- Métodos de consulta vistos: `QueryListingsForItem`, `Search`, `SearchDescriptions`, `QueryDescription`, `QueryDescriptionByClassID`, `QueryPriceHistory`, `QueryRecentSales`. Mutações: `UpdateSellListingPrice`, `CancelSellListing`, `AddListingToMarketWatchlist` etc. Livro: `/market/orderbook` método `Load`.
- Caminhos REST antigos ainda referenciados no bundle: `/market/buylisting/${id}`, `/market/createbuyorder/`, `/market/cancelbuyorder/`, `/market/getbuyorderstatus/`, `/market/removelisting/${id}`, `/market/userbillinginfo`, `/market/advancedsearchdata`.

**`QueryListingsForItem`** (testado anônimo, HTTP 200, moeda BRL):
```
qp=[{"appid":730,"strItemName":"G180420B6053004","filters":{},"accessoryFilters":{},
     "propertyFilters":{"1":{"property_id":1,"int_min":"58","int_max":"66"}},
     "sort":{"field":1,"direction":0,"assetpropertyid":2},"start":0}]
```
- `strItemName` = **código G** (agrupa todas as wears + StatTrak + Souvenir; Moonrise = 5111 anúncios). O nome comum não funciona: veio `total_count: 0`.
- Página fixa de **20 anúncios**: pedi `count:100` e vieram 20. A paginação usa `start` e `more`/`total_count`.
- `propertyFilters` (mensagem protobuf vista no bundle): `{property_id, float_min, float_max, int_min, int_max}`. **`int_min`/`int_max` precisam ser strings** (int64). Com número cru o filtro falha em silêncio e devolve lixo (veio 1 resultado com seed 0). Seed 59 → `total 9` anúncios. Float 0–0,02 → 51.
- `sort.field`: `0` = preço, `1` = propriedade (exige `assetpropertyid`). `direction` 0/1. **Não existe ordenação por "mais recente"** nesse método.
- Campos de cada anúncio: `listingid, unPrice, unFee, unSteamFee, unPublisherFee, unPricePerUnit, unFeePerUnit, unSteamFeePerUnit, unPublisherFeePerUnit, publisherFeeApp, publisherFeePct (0.1), eCurrency (7=BRL), strSubtotal ("R$5.88"), bMine, enhanced_appearances[] (imagens avif), description{...}, asset{...}`.
- `unPrice` = o que o vendedor recebe; o comprador paga `unPrice + unFee`. Exemplo: 512 + 76 = R$5,88.
- `asset`: `{id, assetid, classid, instanceid, amount, appid, contextid, asset_properties:[...], asset_accessories:[...]}`. Exemplo real:
  `[{"propertyid":1,"int_value":"151"},{"propertyid":2,"float_value":0.2616954445838928},{"propertyid":6,"string_value":"BCAC4B72...2F00"}]`
  → **`int_value` vem como string**; o float vem em float32 promovido a double.
- `description.market_actions[0].link = "steam://run/730//+csgo_econ_action_preview%20%propid:6%"`: o link de inspect é um template que se completa com o hex da propriedade 6.

### 1.4 Esquema de propriedades (primário, sem chave)
`GET https://api.steampowered.com/IEconService/GetAssetPropertySchema/v1/?appid=730&language=english` (acesso 2026-11-03):
| id | name | type | label |
|---|---|---|---|
| 1 | #attr_set_item_texture_seed | 2 (int) | Pattern Template |
| 2 | #attr_set_item_texture_wear | 1 (float) | Wear Rating |
| 3 | #attr_keychain_slot_seed | 2 | Charm Template |
| 4 | #attr_sticker_slot_wear | 1 | Sticker Scrape Level |
| 5 | #attr_custom_name_attr | 3 (string) | Name Tag |
| 6 | #attr_econ_action_preview | 3 | Item Certificate |
| 7 | #attr_set_item_texture_prefab | 2 | Finish Catalog |

Protobuf (dump do webui): `CEconItem_AssetProperty {propertyid=1; int64 int_value=2; float float_value=3; string string_value=4}`, `CEconItem_AssetAccessory {classid, instanceid, standalone_properties, parent_relationship_properties, nested_accessories}` — https://github.com/SteamTracking/Protobufs/blob/master/webui/service_econ.proto (acesso 2026-11-03).

### 1.5 Hex do certificado (adesivos e chaveiros offline)
- CSFloat, 03/2026: "As of March 2026, all CS2 inspect links self-encode the item details"; o repositório `csfloat/inspect` foi arquivado em 2026-03-25 — https://github.com/csfloat/inspect
- Decoder: `@csfloat/cs2-inspect-serializer` (`decodeHex`, `decodeLink`) — https://github.com/csfloat/cs-inspect-serializer (atualizado em 2026-10-08, segundo https://github.com/orgs/csfloat/repositories).
- A extensão do CSFloat (PR #373, merge em 2026-03-14) troca `%propid:6%` pelo `string_value` da propriedade 6 e decodifica localmente — https://github.com/csfloat/extension/pull/373
- Formato: o 1º byte é a chave XOR (0xE3 em links nativos, 0x00 em links gerados por ferramenta), depois vem o protobuf `CEconItemPreviewDataBlock` e um checksum. Fontes: https://packagist.org/packages/vlydev/cs2-masked-inspect , https://pkg.go.dev/github.com/vlydev/cs2-masked-inspect-go/cs2inspect (v1.1.5, 05/2026), https://pypi.org/project/cs2-inspect-lite/
- Para o sniper: **seed e float já chegam prontos nas propriedades 1 e 2.** O hex só faz falta para adesivo/chaveiro, e para esses `asset_accessories` pode bastar (**HIPÓTESE**: não vi nenhum anúncio com adesivo na amostra).
- Fade %, Blue Gem % e fase de Doppler **não** vêm do Steam. Fase sai do `paint_index`, que vem no hex; % de fade ou de azul precisa de tabela externa (seção 5).

---

## 2. Limites de taxa e histórico de preço

### 2.1 Limites (comunidade; a Valve não publica)
- Bloqueio por IP desde ~10/2022 ("You have made too many requests recently"). Um punhado de requisições por minuto já pode render 429, às vezes por horas — https://www.steamwebapi.com/steam-429-error (fornecedor pago, viés comercial).
- Recomendação de partida: 15–20 req/min, ou 100 por 5 min — https://www.mintlify.com/AjaXsb/Hridaya_Steam_Market_Tracker/configuration/config-reference
- `priceoverview` é mais restrito que a busca. IP de datacenter toma 429 quase sempre. A pessoa usou 4 chamadas concorrentes com ~350 ms de pausa. 429 repetido pode virar 403 — https://dev.to/0xgollum/what-breaks-when-you-scrape-steam-community-market-at-scale-200c (~07/2026).
- Fork do steampy: `TooManyRequests` com mais de 20 chamadas em 60 s — https://github.com/xffchao/steampy
- **Não encontrado:** nenhum número publicado para `/market/actions`. A resposta não traz header de rate limit (sonda local). **HIPÓTESE:** mesmo balde por IP do mercado antigo. Logado vs anônimo: nenhuma fonte com medição; **HIPÓTESE** de que logado não aumenta o limite de leitura.
- **Não rodei teste de rajada**, para não queimar o IP de produção do Erick.

### 2.2 Histórico de preço
- Antigo: `/market/pricehistory/?appid=&market_hash_name=` exige sessão logada (`steamLoginSecure`) — https://github.com/Laxnring/csgoprices , https://github.com/HilliamT/scm-price-history
- **Novo (sonda local): `q=QueryPriceHistory&qp=[730,"Glock-18 | Moonrise (Field-Tested)"]` responde sem login**, com `{"ecurrency":7,"prices":[{"time":unix,"price_median":float BRL,"purchases":n}]}`. São 3895 pontos desde 2018: diários no começo, **horários nos últimos ~30 dias** (horário a partir de 2026-10-05, 721 pontos). A página do item também já vem com o histórico embutido.
- `QueryRecentSales`: 50 vendas globais de vários apps (`listingid, rtSold, strSoldPrice`), sem float nem seed. Não serve para preço por item.
- Livro: `GET /market/orderbook?q=Load&qp=[730,"<hash name>"]` (header queryAction) → `amtMaxBuyOrder, amtMinSellOrder, eCurrency, cBuyOrders, cSellOrders, rgCompactBuyOrders[preço,qtd,...]`. O front revalida a cada 5 s (`refetchInterval:5e3` no bundle). Medido em 05/11/2026 sem login (fixture `steam_orderbook_negev_2026-11.json`): o `data` do envelope é `{"success":true,"data":{…}}`, valores em centavos BRL.
- **HIPÓTESE:** a moeda BRL sem login vem da geolocalização do IP (o cookie devolvido foi `timezoneName=America/Los_Angeles`, então não é por fuso).

---

## 3. Compra: `buylisting`, confirmação, travas

### 3.1 Formulário (fonte primária: bundle do mercado novo, 2026-11-03)
`POST https://steamcommunity.com/market/buylisting/<listingid>` como `FormData` (multipart), `credentials: include`:
```
sessionid, currency (= wallet_info.wallet_currency; BRL=7), subtotal (= unPrice),
fee (= unFee), total (subtotal+fee+taxa), tradefee_tax (taxa local), quantity=1,
first_name, last_name, billing_address, billing_address_two, billing_country,
billing_city, billing_state, billing_postal_code, confirmation ("0" ou id), save_my_address ("0"/"1")
```
- No código: `subtotal:r.unSubtotal, fee:r.unFeeAmount, total:r.unTotal, tradefee_tax:r.unTaxAmount`. A equivalência `unSubtotal = unPrice` é dedução minha (**HIPÓTESE** forte, coerente com a forma antiga `converted_price`/`converted_fee`).
- O front pede `/market/userbillinginfo` e só cobra endereço quando `require_billing_info`. **HIPÓTESE:** o Brasil não exige; mandar os campos vazios como o front faz.
- Forma antiga (bot Node que funcionou por anos): `currency=6&subtotal=346&fee=51&total=397&quantity=1&billing_state=&save_my_address=0` — https://freelancehunt.com/en/project/vipravlennya-pomilki-pokupki-predmeta-na-steamcommunity-com-market/1480183.html
- Headers: cookies `steamLoginSecure` e `sessionid`; o form `sessionid` tem de ser igual ao cookie. steampy manda `Referer: /market/listings/<app>/<nome>` — https://github.com/bukson/steampy/issues/447 . **HIPÓTESE:** mandar também `Origin: https://steamcommunity.com`, que o navegador manda sozinho no fetch.

### 3.2 Confirmação (mudança de ~09/07/2025)
- steampy #447 (aberta 2025-07-09, fechada 2025-07-11): a resposta pode vir com `{"success":22,"need_confirmation":true,"confirmation":{"confirmation_id":...}}`. Sucesso traz `wallet_info.success == 1`. A solução que funcionou: mandar `confirmation:'0'`, aprovar a confirmação e reenviar com `confirmation=<id>` depois de ~1 s. Depois disso, outras compras na mesma sessão não pediram confirmação. Erro visto: "too many purchases without confirmation", que exigiu esperar ~5 min — https://github.com/bukson/steampy/issues/447
- O bundle novo faz o mesmo: em `need_confirmation` guarda `{id, cAttempts}`. Se `confirmation_type == 2` (celular), chama o fluxo de confirmação móvel com tipo **12**; senão é por e-mail. Se reenviar com id e falhar, mostra `#PurchaseDialog_Error_NoConfirmation`. Confirmações de compra no app móvel: `int_type 12`, `creator_id == listingid` (#447).
- Ordem de compra (`createbuyorder`) ganhou o mesmo campo `confirmation` (setembro/2025) — https://github.com/bukson/steampy/issues/451
- Na prática: o painel faz o POST. Se vier `need_confirmation`, avisa "confirme no app Steam" e reenvia com o id, em poucas tentativas. Confirmar sozinho exige `identity_secret` (risco de segurança; decisão do Erick).

### 3.3 Erros conhecidos (fóruns Steam; o JSON exato só aparece no steampy)
- "There was a problem purchasing your item. The listing may have been removed. Refresh the page and try again." Aparece quando o anúncio já foi vendido, quando o servidor está sobrecarregado, ou quando o **saldo não cobre a taxa** (o total precisa incluir `tradefee_tax`) — https://steamcommunity.com/discussions/forum/1/2270320616978944732/ , https://steamcommunity.com/discussions/forum/1/691994126364712079/
- "You cannot buy any items until your previous action completes": compras seguidas rápido demais (mesmas threads).
- O texto do erro vem em `message`. **HIPÓTESE:** junto com HTTP 502 nas falhas, como era no mercado antigo.
- Elegibilidade da conta: `EconMarket.IsMarketplaceAllowed` devolve `allowed, reason, allowed_at_time, steamguard_required_days, new_device_cooldown_days` — https://github.com/SteamTracking/Protobufs/blob/master/webui/service_econmarket.proto

### 3.4 Trade Protection e travas de 7 dias
- Trade Protection (lançado 2025-07-15/16): item recebido **por troca** fica 7 dias protegido, não pode ser vendido no Mercado nem trocado de novo, e a troca pode ser revertida. A reversão desfaz todas as trocas protegidas dos últimos 7 dias e bloqueia a conta 30 dias para troca e Mercado — https://csdb.gg/cs2-trade-protection-update/ , https://itrade.gg/blog/trade-protection-in-CS2 , texto da Valve espelhado em https://notes.thourcs2.com/
- **Compra no Mercado:** a própria descrição do anúncio traz `"market_tradable_restriction":7,"market_marketable_restriction":7` (sonda local, primária). Notas do CS2 de 2024-10-02, citadas em https://csrated.com/guides/steam-trade-hold-explained/ : "All items acquired from the in-game store (including the Armory), Trade Offers, and Steam Community Market are now subject to a 7 day re-trade and re-market restriction". Também https://swap.gg/blog/cs2-7-day-trade-lock-explained (2026-04-14).
- Compra no Mercado é final, sem reversão — https://tech-insider.org/how-cs2-skin-marketplaces-handle-trust-escrow-dispute-resolution/ (paráfrase da Valve; **HIPÓTESE** sem fonte da Valve direta). Itens de CS2 não sofrem trade hold/escrow — https://skintrain.gg/en/blog/steam-trade-hold-15-days
- Consequência: cada flip deixa o capital preso ≥7 dias, com risco de preço nesse intervalo. A "venda rápida" estimada precisa ser o preço daqui a 7 dias, e não o de agora.

---

## 4. Taxas (CS2, 2026)

- 5% Steam + 10% do publisher (Valve), calculados **sobre o valor que o vendedor recebe**, cada parte arredondada **para baixo**, mínimo de 1 centavo por parte. Fórmula: `steam=floor(r*0.05)`, `pub=floor(r*0.10)`, `comprador=r+steam+pub` — https://casehint.com/guides/steam-market-fees , https://www.skinwise.gg/learn/steam-market-fees-explained , https://csbeacon.com/blog/steam-market-fees-explained
- **Confirmado com dados vivos em BRL (sonda local):** `unPrice 512 → unSteamFee 25, unPublisherFee 51` (floor 25,6 e 51,2); `523 → 26 + 52`. `publisherFeePct: 0.10000000149011612`.
- Quem vende recebe ≈ 86,96% do que o comprador paga; abaixo de ~US$0,20 o mínimo de 1 centavo empurra a taxa efetiva para cima. Nenhuma fonte de 2026 cita mudança no 5%+10%.
- Para inverter (dado o preço do comprador `B`, achar `r`): procurar o maior `r` com `r+floor(.05r)+floor(.10r) ≤ B`. Uma divisão por 1,15 erra por 1 centavo. Precisa de teste.
- Preço mínimo por anúncio: **HIPÓTESE** de R$0,03 (1+1+1 centavo); não confirmado.

---

## 5. Como precificar padrão raro

- **Vendas reais do CSFloat (primária, sonda local):** `GET https://csfloat.com/api/v1/history/<market_hash_name>/sales` **responde sem chave** com 40 vendas (Moonrise FT: 2026-10-29 a 2026-11-03). Cada venda traz `price` (centavos de USD), `reference{base_price, float_factor, predicted_price, quantity, sticker_overpay}`, `item{paint_seed, float_value, paint_index, serialized_inspect, ...}`. Seeds T1 vendidas: 224 a US$1,05, 601 a 1,49, 58 a 1,50, 864 a 1,22, com `base_price` 0,73. Prêmio T1 ≈ 1,4–2x.
- O `reference` do CSFloat tem fator de float e de adesivo, mas **não tem fator de padrão**: o prêmio de padrão tem de vir de vendas por seed.
- Blogs (secundários, falam de preço pedido e não de venda): T1 da Moonrise "a partir de US$100" — https://skin.land/blog/full-glock-18-moonrise-patterns-guide/ ; #694 "US$100 a quase US$1000" — https://hellcase.com/blog/skinopedia/glock-18-moonrise-patterns/ . **Não usar como preço.**
- **Tiers da Moonrise** (consenso entre https://lis-skins.com/blog/best-glock-18-moonrise-pattern-tier-list-guide/ , https://csmarketcap.com/blog/esports/glock-18-moonrise-guide-rare-star-pattern-seeds , https://www.about-cs.online/glock-18-moonrise-star-pattern-guide/):
  - T1 (estrela central): 58, 59, 66, 90, 102, 224, 601, 628, 694, 706, 837, 864
  - T2: 19, 109, 142, 165, 194, 237, 302, 403, 475, 487, 509, 800, 958
  - T3: 31, 38, 95, 121, 281, 355, 385, 484, 617, 630, 667, 737, 913
  - T4: 137, 268, 448, 744, 769, 986
  - Divergência: um guia antigo põe 958 em T1 — https://steamcommunity.com/sharedfiles/filedetails/?id=2535064032
  - O padrão não depende da wear; o float da Moonrise vai de 0,00 a 0,65.
- Case Hardened (Blue Gem): vendas por padrão em https://app.csgobluegem.com/case-hardened-cs2-sales-data (API paga; wrapper https://github.com/fretgfr/csbluegem.py com `pattern`, `playside_blue`, `backside_blue`); https://bluegemlab.com/ ; ranking de azul por seed em https://csdb.gg/pattern-database/ . Esquema de tiers por % de azul em https://cs2floatchecker.com/blog/pattern-guide (2025). Nenhum dataset JSON aberto e confiável encontrado (**HIPÓTESE** de que não existe).
- Doppler e Gamma Doppler: a fase sai do `paint_index` e não do seed. Ruby 415, Sapphire 416, Black Pearl 417, fases 1–4 = 418–421 (há exceções por faca); Gamma: Emerald 568, fases 569–572 (a Glock tem índices próprios) — https://cspriceapi.com/resources/cs2-doppler-phases-price-guide
- Fade %: vem de calculadora/tabela por seed (o CSFloat e o CSDB têm as suas); nenhuma tabela aberta verificada. **HIPÓTESE.**
- Pricempire: a API não tem prêmio de padrão, só índices; planos de US$119,90/mês (10k chamadas) e US$239,90/mês. Gratuito: 1000 chamadas/mês — https://pricempire.com/api , https://pricempire.com/subscribe
- Buff163: **não verificado** nesta pesquisa.

### 5.1 API do CSFloat (2026)
- Base `https://csfloat.com/api/v1`, header `Authorization: <API-KEY>` (sem "Bearer"); a chave se gera no perfil, aba "developer" — https://docs.csfloat.com/ (DNS falhou em 2026-11-03; conteúdo via busca).
- `GET /listings` com filtros `paint_seed`, `paint_index`, `min_float`, `max_float`, `def_index`, `market_hash_name`. **Sem login → 403 "You need to be logged in to search listings"** (sonda local).
- Limites (headers da sonda local): `/listings` `x-ratelimit-limit: 50000`; `/history/.../sales` `x-ratelimit-limit: 500`, com reset ~21 h à frente. **HIPÓTESE:** a janela é diária e por IP; o `remaining 414` sugere que algo neste IP já consome essa cota. Limites por IP (IPv4 e IPv6 separados) e por conta/endpoint — https://pkg.go.dev/github.com/Bios-Marcel/csfloat_go (2026-08-24).
- Custo: chave gratuita (**HIPÓTESE**: não vi tabela de preço). Taxa de venda de 2% — https://hrt.github.io/csfloat/ (documentação não oficial, gerada por IA).

---

## 6. Snipers e bots open source

Nenhum usa o mercado novo. Lições:
- https://github.com/FatSquare/cs2_market_botX (Python): só lê float e preço, não compra. A moeda muda conforme o cookie; cookie expirado devolve moeda errada.
- https://github.com/sotolko/csgo-market-sniper (~2022): compra por padrão, float e adesivo via navegador Chromium. Obsoleto.
- https://github.com/ntaklive/csgo_market_bot : compra por float (antigo).
- https://github.com/ErayBahar/steammarketscraper (Python): usa `itemordershistogram` e alerta no Telegram quando a venda fica abaixo da ordem de compra. Post: https://medium.com/@eraybahar24/i-tried-building-a-steam-market-sniper-bot-as-a-beginner-heres-what-i-learned-ced6b1005946
- https://github.com/Soniclev/steam_csfloat_rust : compara CSFloat com Steam (Rust pelo nome; atividade **não verificada**).
- https://github.com/somespecialone/aiosteampy (v0.7.21, 2026-03-12): o README avisa "Beta Steam Market methods ... will replace old when Steam migrate their website"; tem confirmações móveis via `identity_secret`.
- Lista curada: https://github.com/redlfox/awesome-cs2-trading ; IDs: https://github.com/somespecialone/steam-item-name-ids
- **Cuidado:** https://github.com/BeamMaharajaManor/cs2-skin-price-sniper distribui `.exe` pronto de conta desconhecida, padrão típico de malware.
- Comercial: CSWatcher (varredura 24/7 em vários marketplaces, auto-compra paga) — https://cswatcher.com/guides
- **Velocidade:** não há websocket nem feed de "novos anúncios" no Mercado Steam. `QueryListingsForItem` só ordena por preço ou propriedade. O caminho viável é sondar cada alvo com `sort` de preço ascendente + `propertyFilters` (seed/float) e diferenciar os `listingid`. Como `propertyFilters` é indexado por `propertyid` (uma faixa por propriedade), seeds T1 espalhadas pedem várias faixas: 58–66, 90–102, 224, 601–628, 694–706, 837–864, ou uma consulta por seed. **HIPÓTESE** de que o filtro aceita várias faixas para a mesma propriedade.
- Armadilhas citadas: 429 virando 403; IP de datacenter; arredondamento de centavos (`0.29 → 28` no steamy-py #22); moeda errada com cookie expirado.

---

## 7. Crates Rust (versões no crates.io em 2026-11-03)

| crate | versão | atualização | nota |
|---|---|---|---|
| reqwest | 0.13.5 | 2026-09-08 | features: `cookies`, `json`, `form`, `query`, `multipart` (`form`/`query`/`multipart` **não** entram no default; default-tls = **rustls**) |
| cookie_store / reqwest_cookie_store | 0.22.1 / 0.10.0 | 2026-02 / 2026-01 | jar persistível para `steamLoginSecure`/`sessionid` |
| axum | 0.8.9 | 2026-04-14 | SSE em `axum::response::sse` (sem feature extra, **HIPÓTESE**: conferir docs.rs) |
| tower-http | 0.7.1 | 2026-08-31 | `ServeDir`/cors se precisar |
| tokio | 1.54.1 | 2026-10-21 | |
| rusqlite | 0.41.1 | 2026-10-28 | feature `bundled` (SQLite embutido, simples no Windows); síncrono, usar `spawn_blocking` |
| sqlx | 0.9.0 | 2026-05-21 | async + migrações; mais pesado; para uma app local de um usuário, rusqlite basta |
| rust-embed | 8.13.0 | 2026-10-07 | embute o frontend estático no binário |
| governor | 0.10.4 | 2025-12-16 | rate limiter (token bucket) |
| prost | 0.14.4 | 2026-06-07 | só se for decodificar o hex do certificado em Rust (proto `CEconItemPreviewDataBlock`) |
| steam-user | 0.2.9 | 2026-07-06 | cliente web Steam por cookie (mercado e confirmações); 106 downloads em 90 dias, imaturo — https://docs.rs/steam-user |
| csfloat-rs | 0.2.0 | 2025-10-30 | não oficial, 9 downloads em 90 dias; não compensa usar |

- Dataset de itens: ByMykel CSGO-API, último commit em 2026-11-03 — `https://raw.githubusercontent.com/ByMykel/CSGO-API/main/public/api/en/skins.json` (2126 skins, 4,7 MB). Campos: `name, weapon{weapon_id}, pattern, min_float, max_float, rarity, stattrak, souvenir, paint_index (string), wears[], collections[]`. A Moonrise tem `paint_index "694"` e float de 0 a 0,65. Hospeda só `en` e `zh-CN`; `pt-BR` só gerando localmente — https://github.com/ByMykel/CSGO-API
- Outro esquema: https://github.com/somespecialone/cs2-items-schema
- Inspect em Rust: nenhum crate maduro encontrado. Decodificar o hex = XOR com o 1º byte + `prost`. **HIPÓTESE**: é pouco código, mas exige teste contra os vetores do cs2-inspect-lite e do vlydev.

---

## 8. Abertos / verificar antes de codar
- Limite de taxa real de `/market/actions` (medir com backoff, nunca em rajada no IP de produção).
- Se `buylisting` aceita anúncio vindo do mercado novo com `subtotal=unPrice` e `fee=unFee` (testar com o item mais barato possível, com o Erick).
- Se `propertyFilters` aceita várias faixas de seed numa consulta só.
- Se a moeda BRL anônima depende do IP (afeta quem rodar fora do Brasil ou via VPN).
- Valor de `tradefee_tax` para conta BR (esperado 0, **HIPÓTESE**).

---

## 9. Login por QR (IAuthenticationService), pesquisado em 2026-11-04
- Fluxo: `BeginAuthSessionViaQR` → `challenge_url` (vira o QR) → `PollAuthSessionStatus` a cada `interval` (5 s) → `refresh_token` + `access_token` + `account_name`. Cookie `steamLoginSecure` = `<steamid>%7C%7C<access_token>` (steamid = `sub` do JWT). Fonte: https://github.com/DoctorMcKay/node-steam-session (`LoginSession.ts`, `AuthenticationClient.ts`, `WebApiTransport.ts`).
- Transporte: `POST https://api.steampowered.com/IAuthenticationService/<Método>/v1/` com `input_protobuf_encoded` (protobuf em base64) num formulário; resposta é protobuf binário com o header `x-eresult` (1 = OK). **Confirmado ao vivo** (2026-11-04): formulário `application/x-www-form-urlencoded` serve (não precisa de multipart) e o Begin respondeu com `challenge_url` `https://s.team/q/…` e `interval` 5.
- Campos (SteamDatabase/Protobufs, `webui/service_authentication.proto`): Begin req `device_details`=3 (`device_friendly_name`=1, `platform_type`=2, `os_type`=3, `gaming_device_type`=4); Begin resp `client_id`=1 (uint64), `challenge_url`=2, `request_id`=3 (bytes), `interval`=4 (float); Poll req `client_id`=1, `request_id`=2; Poll resp `new_client_id`=1, `new_challenge_url`=2, `refresh_token`=3, `access_token`=4, `account_name`=6; GenerateAccessTokenForApp req `refresh_token`=1, `steamid`=2 (fixed64), `renewal_type`=3; resp `access_token`=1. Enums (valores da biblioteca): plataforma MobileApp = 3; renovação None = 0.
- Renovação: `GenerateAccessTokenForApp` só funciona com refresh token de **MobileApp** (WebBrowser dá AccessDenied, doc da biblioteca, 2025-04-30); o `aud` do token mobile é `["web","mobile"]`, então vale como cookie no steamcommunity.com. Por isso o QR é pedido como MobileApp. A rota WebBrowser (`login.steampowered.com/jwt/finalizelogin` + `settoken` em cada domínio) existe, mas exige várias chamadas por renovação.
- **HIPÓTESE (não medida):** que o cookie vindo de token mobile também seja aceito em `buylisting`; e que o QR vale ~5 min (a Steam não documenta).
- **Verificação de elegibilidade (log real, 2026-11-04):** com login válido, `GET /market/` responde 302 para `/market/eligibilitycheck/`, que seta o cookie `webTradeEligibility` e volta a `/market/`. Fontes do comportamento e do nome do cookie: https://github.com/dmitryshelamov/ProfitGraph/pull/112 e https://github.com/ill-yes/steam-bee/pull/61 (seguir até 3 redirecionamentos na mesma origem e guardar o cookie); https://dev.doctormckay.com/topic/5837-webtradeeligibility-cookie-and-its-purpose/ (é só cache do estado; a Steam rechecagem na compra). Nosso cliente segue 2 saltos e manda o cookie em toda requisição autenticada.

---

## 10. Recuperação pela interface antiga (sondas locais de 06/11/2026)

- Referência da opção `bMarketOptOut=1`: https://github.com/Revadike/InternalSteamWebAPI/issues/13 (relato de maio/2026). A interface pública do mercado também oferece "Sair do beta do Mercado". O SteamCrawl de 2023 não implementa controle especial de 429: https://github.com/Hungreeee/steamcrawl/blob/main/steamcrawl/request.py .
- No IP de produção, actions de anúncios e a página nova responderam 429. Pedidos sem compressão, com `identity`, com combinações normais de compressão e com sessão anônima inicializada continuaram recusados. **Não há prova de que mudar cabeçalhos resolva nosso bloqueio.** O navegador embutido abriu a página; não foi possível comprovar que a saída de rede dele coincide com a do programa, então isso não diagnostica o cliente.
- O JS público atual confirma o formato já usado pelo app: GET `/market/actions?q=<método>&qp=<array JSON>` e `x-valve-request-type: queryAction`. Fontes da própria Steam: https://cdn.akamai.steamstatic.com/steamcommunity/public/ssr/Cr5iF9_D.js e https://cdn.akamai.steamstatic.com/steamcommunity/public/ssr/Fo4SpwBP.js .
- A sonda abaixo respondeu **200**, sem login, com 20 anúncios da Moonrise FT, enquanto as actions continuavam em 429:

```text
GET /market/listings/730/<nome completo escapado>/render/
    ?query=&start=0&count=20&country=BR&language=english&currency=7
Cookie: bMarketOptOut=1
Accept: application/json
```

- Resposta: `success`, `start`, `pagesize`, `total_count`, `listinginfo` indexado por `listingid`, `assets["730"]["2"][assetid]`. O preço do comprador em BRL é `converted_price + converted_fee`, **somente** com `converted_currencyid=2007`. O `price`/`fee` original pode estar em outra moeda (na amostra, `currencyid=2006`): nunca usar esse valor como real.
- O asset contém `market_hash_name`, `market_bucket_group_id` (código G), `asset_properties` e o template de inspect. Seed vem em `int_value` textual; float vem em `float_value` **textual**. A identidade completa, o mapa de assets, a taxa e a moeda são validados antes de aceitar a página. Mapa JSON não garante ordem de preço: ordenar após converter.
- Captura pública recortada, com três anúncios e seus assets: `testdata/steam_legacy_render_moonrise_2026-11.json`. Recorte conserva os campos originais de paginação; não é uma página completa para medir cobertura.
- `GET /market/pricehistory/?appid=730&market_hash_name=<nome>` com a opção antiga respondeu **400 sem login** e **200 com a sessão já salva do app**, com 3.898 pontos. Resposta: `success=true`, `price_prefix="R$"`, `price_suffix=""`, `prices=[["Feb 16 2018 01: +0",48.186,"522"],...]`. Prefixo de moeda e UTC `+0` são obrigatórios no leitor. Captura pública recortada: `testdata/steam_legacy_history_moonrise_2026-11.json`. Nenhuma credencial foi salva nas capturas ou impressa.
- **Limites:** nenhuma cota maior foi medida. O fallback compartilha a fila conservadora de busca/livro e respeita 429, com pelo menos 3 s entre quaisquer pedidos pela saída principal. Não há rotação de cabeçalhos, contas ou proxies para multiplicar cotas. Filtros de seed/float continuam dependendo das actions; os primeiros anúncios do nome não representam a cobertura de um filtro remoto.
- O HTML da página nova que veio com 429 tinha só estrutura e preferências, sem anúncios/histórico: não foi aceito como dado. O fallback SSR não foi implantado, pois a alternativa JSON funcionou e permite reutilizar os validadores existentes.

## 11. Saída Starlink: recusa das actions, leituras públicas disponíveis (06/11/2026)

- Comparação controlada de 11h27–11h28 (Fortaleza), com o v2 pausado e religado ao terminar. Seis GETs HTTPS, sem login, sem seguir redirecionamentos e com pelo menos 6 s entre consultas. O mesmo Curl, URL de `QueryListingsForItem`, código G, desgaste e cabeçalhos nos dois caminhos retornou **200 com 20 anúncios pela principal** e **429 pela Starlink**. O 429 tinha corpo vazio, `server: nginx` e nenhum `Retry-After`; não informa prazo nem o critério de limitação.
- Na mesma saída Starlink, `/market/` retornou 200, o `/render/` da seção 10 retornou 200 com 20 anúncios e `converted_currencyid=2007`, e `/market/orderbook?q=Load&qp=[730,<nome>]` retornou 200 com `eCurrency=7`. Os corpos do render nas duas saídas foram idênticos (SHA-256), sem sessão. Assim, a falha não era uma indisponibilidade geral do celular ou um limite de todas as leituras da Steam.
- O proxy local só responde 405/503/502/500 em falhas de CONNECT; depois de CONNECT 200, copia os bytes da conexão TLS. O cliente só registra 429 da extra ao receber o status HTTP 429. A reprodução por Curl fora do app confirma a recusa dependente da rota para as actions; não foi encontrado erro do app que fabricasse esse status.
- Os IPs públicos das duas saídas eram diferentes, e a extra confirmou `AS14593` (Starlink). A [documentação oficial da Starlink](https://starlink.com/pl/support/article/1192f3ef-2a17-31d9-261a-a59d215629f4) informa CGNAT no IPv4 padrão. **Hipóteses ainda não provadas:** cota compartilhada, reputação de IP/rede ou outra regra interna da Steam. A captura não permite atribuir o bloqueio a outros assinantes nem concluir o limite exato. Desligar o celular não garante trocar o IP público ou limpar estado do servidor.
- Na consulta DNS local, `steamcommunity.com` e `api.steampowered.com` não tinham registros AAAA. Não foi inventada uma saída IPv6 nem alterado roteador, IP ou sessão para tentar contornar a recusa.
- Correção operacional: o proxy Starlink da produção atende **livro de ofertas e anúncios antigos públicos**. Actions, histórico autenticado, sessão, login e compra ficam na principal. O limitador extra continua conservador e respeita seu próprio 429 e falhas de rede. Anúncios comuns podem aproveitar a fila pública livre durante a espera da principal, sem representar cobertura de filtros remotos. A causa interna da regra da Valve permanece opaca; ganho de frequência precisa de medição após instalar.

## 12. Primeiro 429 logado sem recuo (registros e teste local de 07/11/2026)

- Às 02h57min47s (Fortaleza), anúncios via actions receberam 429 logado; às 02h57min59s, a tentativa sem login também recebeu 429. Às 07h00min04s e 07h00min26s, o mesmo ocorreu com histórico após renovar o login. Os registros também mostram recusas anônimas a cada 30 min por horas, enquanto livro/render continuaram respondendo. Isso não identifica a cota interna da Valve nem prova que o ritmo recente sozinho causou o bloqueio.
- Causa comprovada no cliente: o primeiro 429 logado retornava antes de `Limiter::on_429`, ignorando a pausa, o alongamento e até um `Retry-After` fornecido pelo servidor. Um servidor local que pede 3600 s reproduziu a falha antes da correção. Agora a troca para anônimo espera o mesmo recuo; uma segunda resposta em voo com prazo maior estende a pausa sem duplicar a escalada. O log registra prazo, autenticação e `Retry-After` sem credenciais. Nenhuma sonda extra foi enviada à Steam durante esta investigação.
- Pública e autenticada são alternativas de acesso, sem prova de cotas independentes: anúncios comuns funcionaram publicamente no render antigo; o histórico antigo exigiu login (seção 10); filtros remotos de seed/float ainda dependem das actions. Não é preciso consultar o mesmo dado nas duas para somá-lo.
- O [RFC 6585, seção 4](https://www.rfc-editor.org/rfc/rfc6585#section-4) define o 429, mas deixa ao servidor como identificar o cliente e contar pedidos. A resposta da Steam não permite concluir se a regra atingida é da conta, do IP, do recurso ou outra combinação; a causa interna continua não demonstrada.

## 13. Rede tinfo.net autorizada na saída do celular (07/11/2026)

- O celular estava online no Tailscale, mas o CONNECT local respondia 503 antes de chegar à Steam: o `saida-celular` aceitava somente `AS14593` (Starlink). Uma nova conferência pela própria saída, às 07h29min40s de Fortaleza, identificou `AS269504 Paulo Dos Santos Teixeira Junior`, na conexão informada pelo Erick como tinfo.net. Isso é recusa da proteção local, não 429 da Steam.
- O auxiliar instalado em `%LOCALAPPDATA%\tailscale-steam` agora aceita lista de redes na opção `-asn`. Seu iniciador foi configurado com `-asn AS14593,AS269504`; nenhuma outra rede foi liberada, e a verificação de saída online continua obrigatória. O log passa a mostrar a primeira rede observada mesmo quando ela é recusada, evitando ocultar uma mudança entre duas redes não autorizadas.
- Código, executável e iniciador anteriores foram preservados em arquivos `*.antes-tinfo.bak`, sem alterar o estado de autenticação do Tailscale. Compilação com a mesma opção do executável anterior (`GOEXPERIMENT=nojsonv2`, `CGO_ENABLED=0`); teste local cobre ambas as redes, espaços, rede desconhecida, prefixo parecido e lista vazia; `go test`, `go vet` e compilação passaram.
- Às 07h32min19s, a proteção liberou a rede nova. Uma consulta HTTPS de IP público por cada saída confirmou IPs diferentes, sem registrar os endereços. Depois de religar o v2, o registro confirmou cinco consultas do livro pela saída extra com sucesso, espaçadas em 6 s, entre 07h33min16s e 07h33min40s. Não foi feito teste extra das actions nessa conexão; permanece sem prova de cota independente ou de ganho de lucro.

## 14. Livro ocupando as vagas da leitura paralela (07/11/2026)

- Entre 07h39min00s e 07h40min00s de Fortaleza, `research_attempts` registrou 20 leituras bem-sucedidas do livro (10 por saída) e nenhuma página de anúncios. A leitura paralela só começou páginas às 07h38min38s e 07h42min26s. Isso explica por que o total de consultas/minuto não representava anúncios avaliados, sem precisar atribuir essas consultas a 429.
- Causa no cliente e na agenda: o leitor só iniciava com vaga imediatamente livre; a triagem do livro reservava quase todas as vagas antes de o laço voltar a essa verificação. Teste local reproduziu a falha após ocupar uma vaga com o livro. A leitura paralela agora reserva a próxima vaga em até 6 s e aguarda fora do laço principal; triagens posteriores usam a principal ou esperam outra vaga. Não há aumento do ritmo por saída nem mudança na régua de compra.
- A contagem de consultas da extra passa a ocorrer no envio, sem contar uma reserva cancelada. Testes locais verificam intervalo entre envios, concorrência do livro, processamento da página aguardada, continuidade da principal, cancelamento por 429 durante a espera e ausência de login na extra. Isso não comprova mais oportunidades lucrativas nem recuperação dos filtros nativos.

## 15. Comuns presos ao intervalo nativo com a extra indisponível (07/11/2026)

- O balanço das 16h08min44s de Fortaleza registrou 83 avaliações, 35 respostas na principal e 14 no livro, sem 429. No banco, a janela correspondente teve 24 páginas nativas, 9 históricos e duas falhas de rede na extra. O piso aprendido salvo das actions era de 18 308 ms; as leituras nativas observadas ficavam espaçadas em cerca de 18 s. O auxiliar da saída extra registrava falhas de contato com o celular. Esses dados não provam lentidão do processamento local nem uma nova cota da Steam.
- O seletor só preferia o render quando havia vaga imediata na extra, ou depois de as actions falharem. Com a extra indisponível e as actions respondendo, um comum esperava a fila nativa mesmo com uma vaga anterior no livro/render da principal. Agora compara essas duas vagas antes de consultar o comum. A agenda também faz essa escolha antes das triagens por livro e busca: elas gastariam a mesma fila que já pode entregar a página completa. Mantém o intervalo e a pausa de cada fila, o espaçamento global e o caminho filtrado dos raros; resposta pública inválida continua voltando para as actions.
- Servidores locais reproduziram a espera indevida e a triagem consumindo a vaga antes das correções. Os testes verificam página válida pela principal antes da vaga nativa, ausência de triagem extra, extra indisponível, ausência de login no render, espera global e recusa de usar o render durante pausa ou suspensão por falhas. O total de anúncios novos depende das páginas e precisa ser medido ao vivo; não foi aumentado o ritmo configurado de nenhuma fila.
- A primeira medição após esses ajustes teve 43 páginas em 600 s, ante 24 na janela original, sem 429; foram 118 anúncios distintos, 78 acima do saldo e 40 avaliações completas. A agenda repetiu 31 leituras de três alvos raros que trouxeram apenas preços acima do saldo. A exclusão já existia na seleção, mas podia esperar até 15 min: observar um piso novo acima do saldo agora invalida essa seleção para o próximo passo. A mesma regra continua exigindo todas as faixas raras vistas e nenhuma vazia antes de excluir o alvo; não se muda a régua de preço nem o aprendizado. Teste local reproduz a permanência indevida de um comum caro e verifica sua exclusão sem esperar a janela periódica.

## 14. Catálogo público e pesquisa de giro/pattern (08/11/2026)

- Metadados: [ByMykel CSGO-API](https://github.com/ByMykel/CSGO-API), `skins.json` com `weapon_id`, `paint_index`, desgastes e limites de float. As versões sem correspondência única de arma/acabamento não recebem código gerado. Os 1.358 códigos G já medidos no banco coincidiram com os metadados públicos em todos os casos comparáveis; isso não prova existência de anúncios das versões novas.
- [CSGO Trader](https://csgotrader.app/prices/): o feed `https://prices.csgotrader.app/latest/steam.json` fornece referências USD `last_24h`, `last_7d`, `last_30d`, `last_90d`. Não são pisos de anúncios nem contagens de vendas. O cabeçalho `Last-Modified` conserva a data da fonte; recebê-lo hoje não rejuvenesce a observação.
- [Market.CSGO, documentação do próprio operador](https://market.csgo.com/pt/api/content/start): o índice `full-history/all.json` mapeia nomes para IDs; os históricos individuais têm `sales7d`/`sales30d` por moeda. USD/RUB/EUR repetem a mesma contagem, portanto não foram somadas. A amostra por nome/desgaste só serve como sinal desse mercado, sem virar giro Steam ou volume de toda a família. A coleta usou no máximo duas consultas por segundo, abaixo do limite publicado de cinco.
- [Sandstorm Max Purple](https://cs2pattern-prices.com/weapons/galil-ar/sandstorm/max-purple), [PAW Stoner Cat](https://cs2pattern-prices.com/weapons/awp/paw/stoner-cat-1), [Kami](https://cs2pattern-prices.com/weapons/five-seven/kami/pussy) e [Seasons](https://cs2pattern-prices.com/weapons/xm1014/seasons/blue-leaf): vendas publicadas motivam pesquisa por seed individual; adesivos, desgaste e amostra pequena impedem tratar o preço do grupo como preço de cada integrante. A fonte informa vendas da Kami 590; o levantamento inicial de apoio foi corrigido após nova leitura. Não importamos preços/multiplicadores publicados.

A curadoria reproduzível e seus limites estão em `catalogo-2026-11-08.md`; os JSONs de apoio preservam as famílias visuais que ainda não merecem consulta rara ativa.

Na verificação instalada, o feed público respondeu `Content-Encoding: gzip` inclusive com pedido
`Accept-Encoding: identity`. O corpo comprimido tinha 542.005 bytes e o JSON descomprimido 3.727.093 bytes.
Sem o recurso gzip do cliente HTTP, a leitura falhava antes de atualizar referências; o cache válido era
preservado. Habilitamos a descompressão nativa do reqwest, conforme a
[documentação do próprio cliente](https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html#method.gzip),
e uma fixture comprimida do JSON real cobre a resposta que ignora identity. O limite de 8 MiB continua
valendo sobre o corpo descomprimido.

## 16. Bloqueio público, ritmo total e reinício (08/11/2026)

- Às 02h50min41s de Fortaleza, a sessão recebeu 429; logo depois, o render público recebeu 429 pela extra e
  pela principal. Em `research_attempts`, a janela Unix `[1794113441,1794117041)` tinha 600 tentativas pela
  principal e 575 pela extra. As marcas são de reserva da tentativa, anteriores ao envio. A coincidência com
  o volume é indício de limitação por janela, não prova de uma cota oficial, da origem exata da contagem ou
  de que só este app consumiu essas redes. A comparação atual das saídas confirmou IPs públicos diferentes,
  sem gravar os endereços. Não foram emitidas sondas adicionais contra a Steam.
- Das 803 leituras públicas bem-sucedidas nessa hora, 360 repetiam o alvo antes de 60 s; havia 46 alvos
  distintos. A fila principal não compartilhava a espera por alvo do leitor paralelo. Agora compartilham o
  último instante de página válida e deixam 60 s entre leituras comuns sem oportunidade ativa. Consultas
  filtradas continuam cobrindo suas próprias faixas e não são substituídas por páginas públicas.
- Falha comprovada no cliente: a pausa/ritmo público, extra e de controle não sobreviviam ao reinício; o
  gancho nem era chamado no 429 público anônimo ou extra. O novo snapshot conserva as quatro filas e os
  estados antigos continuam legíveis. A migração usa recusas públicas recentes do próprio registro para não
  reabrir essas filas durante a penalidade. Um teste local reproduziu a falta do aviso de persistência antes
  da correção; os testes verificam restauração, separação das filas, `Retry-After` de 3600 s e migração.
- O envio real agora tem um piso agregado de 10 s entre leituras por saída, inclusive ao alternar endpoints
  e após reiniciar. Isso corresponde a aproximadamente 360 leituras/h por saída, abaixo do volume observado
  antes do bloqueio, sem tratar esse número como regra da Valve. A fila de compra/login preserva o próprio
  ritmo. Testes locais verificam os intervalos reais recebidos por um servidor e a prioridade das ações do usuário.
- O [RFC 6585, seção 4](https://www.rfc-editor.org/rfc/rfc6585#section-4) define o 429, mas deixa a identificação
  do cliente e a forma de contar pedidos ao servidor. As causas internas e o prazo final da restrição da Steam
  permanecem opacos. A proteção corrige o tráfego e o reinício do app; não promete impedir qualquer restrição
  futura causada por tráfego externo ou mudanças da plataforma.
- Com o v2 parado das 03h02min04s às 03h26min16s de Fortaleza, as primeiras respostas de sessão, histórico e
  render extra ainda foram 429. Isso confirma que uma pausa de cerca de 24 min do programa não bastou nesta
  ocorrência, sem revelar o prazo interno da Steam nem excluir tráfego externo. O cliente passa a aplicar
  pelo menos 30 min de descanso nas recusas públicas, preservando `Retry-After` maior. Quando as duas filas
  de leitura da principal já estão recusadas, alinha seus prazos pelo maior; não propaga a recusa a uma fila
  saudável ou às ações explícitas do usuário. A conferência de carteira em segundo plano também aguarda
  esse descanso integral. Testes locais verificam a pausa pública longa, o alinhamento condicional e a
  diferença entre verificação automática e explícita da sessão.
- Às 03h58 de Fortaleza, a principal ainda recebeu 429 após cerca de 30 min sem leituras do v2; a extra
  também continuou recusada. O prazo interno segue desconhecido. Repetir sempre a pausa de 30 min não
  recuperou esta ocorrência. A política pública agora começa em 30 min e dobra para 60 min e 2 h em novas
  recusas após o descanso; esses valores são uma decisão conservadora do cliente, não limites publicados
  da Steam. A sequência só zera após 4 h sem 429. Testes com relógio injetado cobrem os degraus, a recusa
  duplicada em voo, `Retry-After` maior que o teto e a restauração do degrau longo após reiniciar. As pausas
  nativas e de controle mantêm a política anterior.
