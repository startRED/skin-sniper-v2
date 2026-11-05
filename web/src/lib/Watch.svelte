<script lang="ts">
  import { app } from './store.svelte';
  import { ago, money } from './format';
  import type { WatchRow } from './types';

  let { collapsed = false }: { collapsed?: boolean } = $props();

  const modes = { barato: 'Barato', seed: 'Seed' } as const;
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
  // Pedidos de compra que compensam, o de maior lucro primeiro. Só sugestão: quem abre na Steam é o Erick.
  // Livro com mais de 1 h some também aqui: a lista pode ter ficado velha se a atualização falhou.
  const bids = $derived(app.watch.filter((w) => w.bid_cents != null && app.now - w.book_at! <= 3600).sort((a, b) => b.bid_profit_cents! - a.bid_profit_cents!));
  const steam = (name: string) => `https://steamcommunity.com/market/listings/730/${encodeURIComponent(name)}`;
  // Quanto o menor anúncio precisa cair para dar lucro; null sem os dois números (max_buy ≤ 0 = nenhum preço dá lucro).
  const gap = (w: WatchRow) =>
    w.floor_cents != null && w.max_buy_cents != null && w.max_buy_cents > 0 ? w.floor_cents - w.max_buy_cents : null;
</script>

{#snippet list()}
  <div class="table" role="table" aria-label="Skins vigiadas">
    <div class="head grid" role="row">
      <div role="columnheader">Skin</div>
      <div role="columnheader">Menor anúncio</div>
      <div role="columnheader" title="Maior preço que ainda dá lucro">Compraria até</div>
      <div role="columnheader" title="Quanto o menor anúncio precisa cair para dar lucro">Falta cair</div>
      <div role="columnheader">Vendas/dia</div>
      <div role="columnheader">Conferido</div>
    </div>
    {#each rows as w (`${w.name}|${w.group ?? ''}`)}
      {@const g = gap(w)}
      <div class="row grid" class:ready={g != null && g <= 0} role="row">
        <div class="c skin" role="cell">
          <img src={w.icon_url} alt="" width="40" height="40" loading="lazy" />
          <span class="names">
            <span class="nm">{w.name}</span>
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
          {:else}{money(g)} <small class="muted">({((g / w.floor_cents!) * 100).toFixed(0)}%)</small>{/if}
        </div>
        <div class="c muted" role="cell" data-l="Vendas/dia">{Math.round(w.sales_per_day)}</div>
        <div class="c muted" role="cell" data-l="Conferido">{w.last_checked_at ? ago(w.last_checked_at, app.now) : 'ainda não'}</div>
      </div>
    {/each}
  </div>
{/snippet}

{#if bids.length > 0}
  <section aria-labelledby="h-bids">
    <h2 id="h-bids">Pedidos de compra que compensam</h2>
    <p class="muted sub">Abra um pedido na Steam com o valor abaixo: ele fica na frente da fila e, se alguém vender para ele, dá esse lucro na venda pelo preço médio.</p>
    <div class="table" role="table" aria-label="Pedidos de compra sugeridos">
      {#each bids as w (w.name)}
        <div class="row bid" role="row">
          <div class="c skin" role="cell">
            <img src={w.icon_url} alt="" width="40" height="40" loading="lazy" />
            <span class="names"><span class="nm">{w.name}</span><small class="reason">vende {Math.round(w.sales_per_day)}/dia · pedidos lidos {w.book_at ? ago(w.book_at, app.now) : '—'}</small></span>
          </div>
          <div class="c" role="cell"><small class="muted">pedir a</small> <b>{money(w.bid_cents!)}</b></div>
          <div class="c" role="cell"><span class="pill ok">+{money(w.bid_profit_cents!)}</span> <small class="muted nowrap">se encher</small></div>
          <div class="c go" role="cell"><a class="btn" href={steam(w.name)} target="_blank" rel="noopener noreferrer">Abrir na Steam</a></div>
        </div>
      {/each}
    </div>
  </section>
{/if}

{#if n > 0}
  {#if collapsed}
    <details class="fold">
      <summary>Vigiando {n} {n === 1 ? 'skin' : 'skins'}</summary>
      {@render list()}
    </details>
  {:else}
    <section aria-labelledby="h-watch">
      <h2 id="h-watch">Vigiando {n} {n === 1 ? 'skin' : 'skins'}</h2>
      <p class="muted sub">As mais perto de dar lucro aparecem primeiro.</p>
      {@render list()}
    </section>
  {/if}
{/if}

<style>
  h2 { font-size: 15px; }
  section { margin-top: 12px; }
  .sub { margin: 2px 0 8px; }
  .fold { background: var(--panel); border-radius: var(--radius); margin-top: 12px; }
  summary { cursor: pointer; padding: 12px 16px; color: var(--muted); }
  summary:hover { color: var(--ink); }
  .grid {
    display: grid; align-items: center; column-gap: 12px; padding: 0 16px;
    grid-template-columns: minmax(260px, 3fr) repeat(2, minmax(90px, 1fr)) minmax(130px, 1.3fr) minmax(70px, 0.7fr) minmax(90px, 0.9fr);
  }
  .head { min-height: 34px; border-bottom: 1px solid var(--line); color: var(--muted); font-size: 12px; }
  .row { min-height: 56px; border-bottom: 1px solid var(--line); }
  .bid { display: grid; align-items: center; column-gap: 12px; padding: 0 16px; grid-template-columns: minmax(260px, 3fr) repeat(2, minmax(110px, 1fr)) auto; }
  .nowrap { white-space: nowrap; }
  .row.ready { background: var(--green-bg); box-shadow: inset 3px 0 0 var(--green); }
  .c { min-width: 0; }
  .c::before { content: attr(data-l); display: none; }
  .skin { display: flex; align-items: center; gap: 10px; }
  .skin img { width: 40px; height: 40px; border-radius: 6px; background: var(--panel-2); object-fit: contain; flex: none; }
  .names { display: flex; align-items: center; flex-wrap: wrap; gap: 2px 8px; min-width: 0; }
  .nm { font-weight: 600; }
  .reason { flex-basis: 100%; color: var(--muted); font-size: 12px; }

  @media (max-width: 899px) {
    .head { display: none; }
    .grid { grid-template-columns: repeat(3, 1fr); row-gap: 8px; padding: 12px 14px; }
    .bid { grid-template-columns: repeat(2, 1fr); row-gap: 8px; padding: 12px 14px; }
    .bid .go { grid-column: 1 / -1; }
    .bid .go .btn { width: 100%; justify-content: center; }
    .skin { grid-column: 1 / -1; }
    .c::before { display: block; color: var(--faint); font-size: 11px; }
    .skin::before { display: none; }
  }
</style>
