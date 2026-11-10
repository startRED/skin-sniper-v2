<script lang="ts">
  import { app } from './store.svelte';
  import { money, signed } from './format';

  const items = $derived((app.state?.combo.ids ?? []).map((id) => app.opps[id]).filter((o) => o && !o.gone_at));
  const combo = $derived(app.state?.combo);
</script>

<!-- Com uma skin só a combinação é a própria linha; a faixa fica enquanto o destaque estiver ligado, para dar como desligar. -->
{#if combo && (items.length > 1 || (app.highlight && items.length > 0))}
  <button class="combo" class:on={app.highlight} aria-pressed={app.highlight} onclick={() => (app.highlight = !app.highlight)}>
    <span class="thumbs" aria-hidden="true">
      {#each items.slice(0, 6) as o (o.id)}<img src={o.icon_url} alt="" width="28" height="28" />{/each}
    </span>
    <span class="txt">Melhor combinação: <b>{items.length} {items.length === 1 ? 'skin' : 'skins'}</b> por <b>{money(combo.cost_cents)}</b>,
      lucro <b class="up">{signed(combo.profit_cents)}</b></span>
    <span class="hint">{app.highlight ? 'Tirar destaque' : 'Destacar na lista'}</span>
  </button>
{/if}

<style>
  .combo {
    display: flex; align-items: center; gap: 12px; width: 100%; text-align: left;
    padding: 8px 14px; border-radius: var(--radius); background: var(--panel);
    border: 1px solid var(--line); color: var(--muted);
  }
  .combo:hover { background: var(--panel-2); }
  .combo.on { border-color: var(--green); background: var(--green-bg); }
  .thumbs { display: flex; flex: none; }
  .thumbs img { width: 28px; height: 28px; border-radius: 6px; background: var(--panel-2); margin-left: -8px; border: 2px solid var(--panel); }
  .thumbs img:first-child { margin-left: 0; }
  .txt { flex: 1; min-width: 0; }
  .txt b:not(.up) { color: var(--ink); }
  .hint { font-size: 13px; white-space: nowrap; color: var(--ink); }
  @media (max-width: 899px) { .hint { display: none; } }
</style>
