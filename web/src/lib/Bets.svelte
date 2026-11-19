<script lang="ts">
  import { api } from './api';
  import { ago, money, signed } from './format';
  import { app } from './store.svelte';
  import type { Opportunity } from './types';

  let open = $state(false);
  let offers = $state<Opportunity[]>([]);
  let loading = $state(false);
  let loaded = $state(false);
  let failed = $state(false);
  const current = $derived(offers.filter(o =>
    o.price_cents <= (app.state?.saldo_cents ?? 0) &&
    (!app.opps[o.id] || app.opps[o.id].gone_at) &&
    app.now - (o.checked_at ?? o.seen_at) < 3 * 3600
  ));
  const tiers = [['Rápido', 'quick'], ['Médio', 'mid'], ['Alto', 'high']] as const;

  $effect(() => {
    if (!open) return;
    const timer = setInterval(refresh, 15_000);
    return () => clearInterval(timer);
  });

  async function refresh() {
    if (loading) return;
    loading = true;
    failed = false;
    try {
      offers = await api<Opportunity[]>('/api/bets');
      loaded = true;
    } catch {
      failed = true;
    } finally {
      loading = false;
    }
  }
</script>

<div class="bets">
  <button class="btn" aria-expanded={open} aria-controls="bets-panel" onclick={() => { open = !open; if (open) void refresh(); }}>
    {open ? 'Ocultar apostas mais arriscadas' : 'Ver apostas mais arriscadas'}
  </button>
  <section id="bets-panel" aria-labelledby="bets-title" hidden={!open}>
    {#if open}
      <div class="head">
        <div>
          <h3 id="bets-title">Apostas mais arriscadas</h3>
          <p class="muted">O preço médio indica lucro. A prova de valor pode ser fraca, e revender rápido pode dar prejuízo.</p>
        </div>
        <button class="btn" disabled={loading} onclick={refresh}>Atualizar apostas</button>
      </div>
      {#if failed}
        <p class="muted" role="alert">Não consegui atualizar as apostas. Tente de novo.</p>
      {:else if loading && !loaded}
        <p class="muted" role="status">Conferindo a lista…</p>
      {:else if current.length === 0}
        <p class="muted" role="status">Nenhuma aposta encontrada nas últimas 3 horas.</p>
      {/if}
      {#each current as o (o.id)}
        {@const checked = o.checked_at ?? o.seen_at}
        {@const stale = app.now - checked >= 600}
        <article>
          <div class="head">
            <div>
              <h4>{o.name}</h4>
              <p>Compra {money(o.price_cents)} <span class="muted">· {stale ? 'sem reconferir' : 'conferido'} {ago(checked, app.now)}</span></p>
              {#if o.group}<p class="muted">{o.group}{o.seed != null ? ` · seed ${o.seed}` : ''}</p>{/if}
            </div>
            <a class="btn" href={o.steam_url} target="_blank" rel="noopener noreferrer">Abrir na Steam</a>
          </div>
          <p class="muted">Preço de revenda e resultado após taxas</p>
          <dl>
            {#each tiers as [label, key]}
              {@const tier = o.sell[key]}
              <div>
                <dt class="muted">{label}</dt>
                <dd>{money(tier.price_cents)}</dd>
                <dd class={tier.profit_cents > 0 ? 'up' : 'down'}>{signed(tier.profit_cents)}</dd>
              </div>
            {/each}
          </dl>
          <p class="muted">{o.evidence}</p>
        </article>
      {/each}
    {/if}
  </section>
</div>

<style>
  section { margin-top: 8px; padding: 16px; border-radius: var(--radius); background: var(--panel); }
  .head { display: flex; align-items: flex-start; justify-content: space-between; flex-wrap: wrap; gap: 8px 16px; }
  .head > div { min-width: 0; overflow-wrap: anywhere; }
  .head > .btn { flex-shrink: 0; }
  h3, h4 { font-size: 16px; margin: 0 0 4px; }
  p { margin: 4px 0; font-size: 14px; }
  article { border-top: 1px solid var(--line); margin-top: 12px; padding-top: 12px; }
  dl { display: flex; flex-wrap: wrap; gap: 8px 20px; margin: 8px 0; font-size: 14px; }
  dd { margin: 0; }
</style>
