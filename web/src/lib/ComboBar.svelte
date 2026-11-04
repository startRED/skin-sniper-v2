<script lang="ts">
  import { app } from './store.svelte';
  import { money, signed } from './format';

  const items = $derived((app.state?.combo.ids ?? []).map((id) => app.opps[id]).filter(Boolean));
  const combo = $derived(app.state?.combo);
</script>

{#if combo && items.length > 0}
  <button class="combo" class:on={app.highlight} aria-pressed={app.highlight} onclick={() => (app.highlight = !app.highlight)}>
    <span class="thumbs" aria-hidden="true">
      {#each items.slice(0, 6) as o (o.id)}<img src={o.icon_url} alt="" width="32" height="32" />{/each}
    </span>
    <span class="txt">
      <b>Melhor combinação com {money(combo.cost_cents)}</b>
      <span class="muted">{items.length} {items.length === 1 ? 'skin' : 'skins'} que cabem no saldo · lucro provável
        <b class="up">{signed(combo.profit_cents)}</b></span>
    </span>
    <span class="hint">{app.highlight ? 'Mostrando na lista' : 'Mostrar na lista'}</span>
  </button>
{/if}

<style>
  .combo {
    display: flex; align-items: center; gap: 14px; width: 100%; text-align: left;
    padding: 10px 14px; border-radius: var(--radius); background: var(--panel);
    border: 1px solid var(--line);
  }
  .combo:hover { background: var(--panel-2); }
  .combo.on { border-color: var(--green); background: var(--green-bg); }
  .thumbs { display: flex; }
  .thumbs img { width: 32px; height: 32px; border-radius: 6px; background: var(--panel-2); margin-left: -8px; border: 2px solid var(--panel); }
  .thumbs img:first-child { margin-left: 0; }
  .txt { display: flex; flex-wrap: wrap; column-gap: 12px; align-items: baseline; flex: 1; min-width: 0; }
  .hint { color: var(--muted); font-size: 13px; white-space: nowrap; }
  @media (max-width: 899px) {
    .hint { display: none; }
    .txt { flex-direction: column; }
  }
</style>
