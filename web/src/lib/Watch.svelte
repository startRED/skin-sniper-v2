<script lang="ts">
  import { app } from './store.svelte';
  import { ago, money } from './format';
  import type { WatchRow } from './types';

  // bids: só os pedidos de compra sugeridos (ficam junto das oportunidades); sem isso, a lista do radar.
  let { bids: onlyBids = false }: { bids?: boolean } = $props();

  const SHOWN = 25;
  let showAll = $state(false);
  let q = $state('');

  const modes = { barato: 'Barato', seed: 'Seed', float: 'Float' } as const;
  type Row = WatchRow & { modes: WatchRow[] };
  // Uma linha por skin (e grupo raro): os modos viram etiquetas. Menor anúncio e "conferido" vêm da mesma
  // consulta (a do menor anúncio; sem nenhum, a mais recente), para não juntar preço velho com horário novo.
  const rows = $derived.by(() => {
    const by = new Map<string, Row>();
    for (const w of app.watch) {
      const key = `${w.name}|${w.group ?? ''}`;
      const r = by.get(key);
      if (!r) { by.set(key, { ...w, modes: [w] }); continue; }
      if (!r.modes.some((m) => m.mode === w.mode)) r.modes.push(w);
      const cheaper = w.floor_cents != null && (r.floor_cents == null || w.floor_cents < r.floor_cents);
      const newer = r.floor_cents == null && w.floor_cents == null && (w.last_checked_at ?? 0) > (r.last_checked_at ?? 0);
      if (cheaper || newer) { r.floor_cents = w.floor_cents; r.last_checked_at = w.last_checked_at; }
      if (w.score > r.score) { r.score = w.score; r.reason = w.reason; }
    }
    return [...by.values()];
  });
  const n = $derived(rows.length);
  const found = $derived.by(() => {
    const term = q.trim().toLowerCase();
    return term ? rows.filter((r) => `${r.name} ${r.group ?? ''}`.toLowerCase().includes(term)) : rows;
  });
  // A busca mostra tudo o que achou; sem busca, só as mais perto.
  const shown = $derived(showAll || q.trim() ? found : found.slice(0, SHOWN));
  // Pedidos de compra que compensam, o de maior lucro primeiro. Só sugestão: quem abre na Steam é o Erick.
  // Livro com mais de 1 h some também aqui: a lista pode ter ficado velha se a atualização falhou.
  const bids = $derived(app.watch.filter((w) => w.bid_cents != null && app.now - w.book_at! <= 3600).sort((a, b) => b.bid_profit_cents! - a.bid_profit_cents!));
  const steam = (name: string) => `https://steamcommunity.com/market/listings/730/${encodeURIComponent(name)}`;
  // Quanto o menor anúncio precisa cair para dar lucro; null sem os dois números (max_buy ≤ 0 = nenhum preço dá lucro).
  // Raro: null, porque o menor anúncio é o da página (qualquer padrão) e o teto é o do grupo raro.
  const gap = (w: WatchRow) =>
    w.kind !== 'raro' && w.floor_cents != null && w.max_buy_cents != null && w.max_buy_cents > 0 ? w.floor_cents - w.max_buy_cents : null;
</script>

{#if onlyBids}
  {#if bids.length > 0}
    <section aria-labelledby="h-bids">
      <h3 id="h-bids">Pedidos de compra que compensam <span class="muted">{bids.length}</span></h3>
      <p class="muted sub">Abra o pedido na Steam por este valor. O lucro vale se alguém vender para ele e você revender pelo preço médio.</p>
      <div class="table" role="table" aria-label="Pedidos de compra sugeridos">
        {#each bids as w (w.name)}
          <div class="row bid" role="row">
            <div class="c skin" role="cell">
              <img src={w.icon_url} alt="" width="32" height="32" loading="lazy" />
              <span class="names"><span class="nm">{w.name}</span><small class="reason">{Math.round(w.sales_per_day)} vendas/dia · livro lido {w.book_at ? ago(w.book_at, app.now) : '—'}</small></span>
            </div>
            <div class="c" role="cell"><small class="muted">pedir a</small> <b>{money(w.bid_cents!)}</b></div>
            <div class="c" role="cell"><b class="up">+{money(w.bid_profit_cents!)}</b> <small class="muted">se encher</small></div>
            <div class="c go" role="cell"><a class="btn" href={steam(w.name)} target="_blank" rel="noopener noreferrer" aria-label="Abrir {w.name} na Steam">Abrir na Steam</a></div>
          </div>
        {/each}
      </div>
    </section>
  {/if}
{:else}
  <section aria-labelledby="h-watch">
    <h2 id="h-watch" class="sr-only">Radar</h2>
    {#if n === 0}
      <p class="muted pad">O radar ainda está escolhendo o que vigiar.</p>
    {:else}
      <div class="tools">
        <p class="muted">{n} {n === 1 ? 'skin vigiada' : 'skins vigiadas'}, da mais perto de dar lucro para a mais longe.</p>
        <input type="search" bind:value={q} placeholder="Buscar skin" aria-label="Buscar skin vigiada" autocomplete="off" />
      </div>
      <div class="table" role="table" aria-label="Skins vigiadas">
        <div class="head grid" role="row">
          <div role="columnheader">Skin</div>
          <div role="columnheader">Menor anúncio</div>
          <div role="columnheader" title="Maior preço que ainda dá lucro">Compraria até</div>
          <div role="columnheader" title="Quanto o menor anúncio precisa cair para dar lucro">Falta cair</div>
          <div role="columnheader">Vendas/dia</div>
          <div role="columnheader">Conferido</div>
        </div>
        {#each shown as w (`${w.name}|${w.group ?? ''}`)}
          {@const g = gap(w)}
          <div class="row grid" class:ready={g != null && g <= 0} role="row">
            <div class="c skin" role="cell">
              <img src={w.icon_url} alt="" width="32" height="32" loading="lazy" />
              <span class="names">
                <a class="nm" href={steam(w.name)} target="_blank" rel="noopener noreferrer" title="Abrir na Steam">{w.name}</a>
                {#each w.modes as m (m.mode)}<span class="pill mute" title="{m.reason} · {m.score.toFixed(2)} centavos de lucro esperado por consulta">{modes[m.mode] ?? m.mode}</span>{/each}
                {#if w.group}<span class="pill {w.kind === 'raro' ? 'warn' : 'mute'}">{w.group}</span>{/if}
                <small class="reason">{w.reason}</small>
              </span>
            </div>
            <div class="c" role="cell" data-l="Menor anúncio"><b>{w.floor_cents != null ? money(w.floor_cents) : '—'}</b></div>
            <div class="c" role="cell" data-l="Compraria até">{w.max_buy_cents != null && w.max_buy_cents > 0 ? money(w.max_buy_cents) : '—'}</div>
            <div class="c" role="cell" data-l="Falta cair">
              {#if g == null}—
              {:else if g <= 0}<span class="pill ok">Dá lucro agora</span>
              {:else}
                {@const p = (g / w.floor_cents!) * 100}
                <span>{money(g)} <small class="muted">({p.toFixed(0)}%)</small></span>
                <span class="meter" class:near={p <= 5} aria-hidden="true"><i style:width="{100 - Math.min(100, p * 4)}%"></i></span>
              {/if}
            </div>
            <div class="c muted" role="cell" data-l="Vendas/dia">{Math.round(w.sales_per_day)}</div>
            <div class="c muted" role="cell" data-l="Conferido">{w.last_checked_at ? ago(w.last_checked_at, app.now) : 'ainda não'}</div>
          </div>
        {/each}
      </div>
      {#if shown.length === 0}<p class="muted pad">Nenhuma skin vigiada com “{q.trim()}”.</p>{/if}
      {#if n > SHOWN && !q.trim()}
        <button class="btn more" onclick={() => (showAll = !showAll)}>{showAll ? `Mostrar só as ${SHOWN} mais perto` : `Mostrar todas as ${n}`}</button>
      {/if}
    {/if}
  </section>
{/if}

<style>
  h3 { font-size: 14px; margin-top: 20px; }
  h3 .muted { font-weight: 400; margin-left: 4px; }
  .sub { margin: 2px 0 8px; font-size: 13px; }
  .pad { padding: 24px 0; }
  .tools { display: flex; align-items: center; justify-content: space-between; gap: 8px 16px; flex-wrap: wrap; margin-bottom: 8px; font-size: 13px; }
  input { width: 240px; max-width: 100%; min-height: 36px; padding: 0 10px; border-radius: var(--radius); background: var(--panel); border: 1px solid var(--line); }
  input::placeholder { color: var(--muted); }
  /* Quanto falta: cheia = já no preço; a escala vai até 25% de queda. */
  .meter { display: block; width: 96px; max-width: 100%; height: 3px; margin-top: 4px; border-radius: 2px; background: var(--panel-2); overflow: hidden; }
  .meter i { display: block; height: 100%; background: var(--faint); }
  .meter.near i { background: var(--green); }
  a.nm { text-decoration: none; }
  a.nm:hover { text-decoration: underline; text-underline-offset: 3px; }
  .grid {
    display: grid; align-items: center; column-gap: 12px; padding: 0 12px;
    grid-template-columns: minmax(280px, 4fr) repeat(2, minmax(90px, 1fr)) minmax(130px, 1.3fr) minmax(70px, 0.7fr) minmax(90px, 0.9fr);
  }
  .head { position: sticky; top: var(--top); z-index: 5; background: var(--bg); min-height: 36px; border-bottom: 1px solid var(--line); color: var(--muted); font-size: 12px; }
  .row { min-height: 44px; padding-top: 5px; padding-bottom: 5px; border-bottom: 1px solid var(--line); }
  .row:hover { background: #12171c; }
  .bid { display: grid; align-items: center; column-gap: 12px; padding-left: 12px; padding-right: 12px; grid-template-columns: minmax(260px, 3fr) repeat(2, minmax(110px, 1fr)) auto; }
  .row.ready, .row.ready:hover { background: var(--green-bg); }
  .c { min-width: 0; }
  .c::before { content: attr(data-l); display: none; }
  .c small { white-space: nowrap; }
  .skin { display: flex; align-items: center; gap: 10px; }
  .skin img { width: 32px; height: 32px; border-radius: 6px; background: var(--panel-2); object-fit: contain; flex: none; }
  .names { display: flex; align-items: center; flex-wrap: wrap; gap: 2px 8px; min-width: 0; }
  .nm { font-weight: 600; }
  .reason { color: var(--muted); font-size: 12px; }
  .more { margin-top: 10px; }

  @media (max-width: 899px) {
    .head { display: none; }
    input { width: 100%; min-height: 44px; font-size: 16px; }
    .grid { grid-template-columns: repeat(3, 1fr); row-gap: 8px; padding: 12px 4px; }
    .bid { grid-template-columns: repeat(2, 1fr); row-gap: 8px; padding: 12px 4px; }
    .bid .go { grid-column: 1 / -1; }
    .bid .go .btn { width: 100%; min-height: 44px; }
    .skin { grid-column: 1 / -1; }
    .c::before { display: block; color: var(--faint); font-size: 11px; }
    .skin::before { display: none; }
  }
</style>
