<script lang="ts">
  import { app, arm, buy, isUncertain, purchaseOf } from './store.svelte';
  import { money } from './format';
  import type { Opportunity } from './types';

  let { o }: { o: Opportunity } = $props();

  const p = $derived(purchaseOf(o.id));
  const status = $derived(p?.status);
  const armed = $derived(app.armed === o.id);
  const otherBusy = $derived(
    app.purchases.some((x) => x.id !== o.id && (x.status === 'pending' || x.status === 'need_confirmation'))
  );
  const uncertain = $derived(isUncertain(p));
  const err = $derived(app.buyError[o.id] ?? (status === 'failed' ? (p?.message ?? 'A compra não deu certo.') : null));
</script>

<div class="buy">
  {#if status === 'pending'}
    <button class="btn" disabled><span class="spin"></span>Comprando…</button>
  {:else if status === 'need_confirmation'}
    <button class="btn warn" disabled>Confirme no app Steam</button>
  {:else if uncertain}
    <button class="btn warn" disabled>Confira seu inventário</button>
  {:else if status === 'bought'}
    <button class="btn ok" disabled>Comprado</button>
  {:else if armed}
    <button class="btn primary armed" onclick={() => buy(o.id)} aria-label="Confirmar compra de {o.name} por {money(o.price_cents)}">
      Confirmar {money(o.price_cents)}<i class="bar"></i>
    </button>
  {:else}
    <button class="btn primary" disabled={otherBusy} title={otherBusy ? 'Espere a compra em andamento terminar' : ''}
      onclick={() => arm(o.id)} aria-label="Comprar {o.name} por {money(o.price_cents)}">
      {err ? 'Tentar de novo' : 'Comprar'}
    </button>
  {/if}
  {#if err}<span class="err" class:amber={uncertain} role="alert">{err}</span>{/if}
</div>

<style>
  .buy { display: flex; flex-direction: column; gap: 4px; align-items: stretch; }
  .btn { width: 100%; position: relative; overflow: hidden; }
  .btn.warn { background: var(--amber-bg); border-color: var(--amber); color: var(--amber); opacity: 1; white-space: normal; line-height: 1.15; padding: 4px 8px; }
  .btn.ok { background: none; border-color: var(--green); color: var(--green); opacity: 1; }
  .armed { background: #fff; border-color: #fff; color: #06281a; }
  .armed:hover:not(:disabled) { background: #fff !important; }
  .bar { position: absolute; left: 0; bottom: 0; height: 3px; width: 100%; background: var(--green); transform-origin: left; animation: drain 4s linear forwards; }
  @keyframes drain { to { transform: scaleX(0); } }
  .spin { width: 12px; height: 12px; border-radius: 50%; border: 2px solid var(--faint); border-top-color: var(--ink); animation: rot 0.8s linear infinite; }
  @keyframes rot { to { transform: rotate(360deg); } }
  .err.amber { color: var(--amber); }
  .err { color: var(--red); font-size: 12px; line-height: 1.25; }
</style>
