<script lang="ts">
  import { app, isUncertain } from './store.svelte';
  import { clock, day, money, until } from './format';
  import type { Purchase } from './types';

  const STATUS: Record<Purchase['status'], [string, string]> = {
    pending: ['mute', 'Comprando…'],
    need_confirmation: ['warn', 'Confirme no app Steam'],
    bought: ['ok', 'Comprada'],
    failed: ['bad', 'Não comprou']
  };
</script>

<section aria-labelledby="h-compras">
  <h2 id="h-compras">Compras</h2>
  {#if app.purchases.length === 0}
    <p class="empty">Você ainda não comprou nada por aqui. Quando comprar, a skin aparece nesta lista com o dia em que ela destrava e os preços para vender.</p>
  {:else}
    <ul>
      {#each app.purchases as p (p.id)}
        <li>
          <img src={p.icon_url} alt="" width="40" height="40" loading="lazy" />
          <div class="who">
            <b>{p.name}</b>
            <span class="muted">Pago {money(p.price_cents)}</span>
          </div>
          <div class="st">
            {#if isUncertain(p)}
              <span class="pill warn">Resultado incerto</span>
              <span class="small amber">{(p.message ?? "").replace(/[.\s]*$/, ".")} Não tente de novo antes de conferir.</span>
            {:else}
              <span class="pill {STATUS[p.status][0]}">{STATUS[p.status][1]}</span>
              {#if p.status === 'failed' && p.message}<span class="down small">{p.message}</span>{/if}
            {/if}
            {#if p.status === 'bought' && p.tradable_at}
              <span class="small muted">Destrava {until(p.tradable_at, app.now)} · {day(p.tradable_at)} às {clock(p.tradable_at)}</span>
            {/if}
          </div>
          <dl class="sell" aria-label="Preços para vender">
            <div><dt>Rápido</dt><dd>{money(p.sell.quick.price_cents)}</dd></div>
            <div><dt>Médio</dt><dd>{money(p.sell.mid.price_cents)}</dd></div>
            <div><dt>Alto</dt><dd>{money(p.sell.high.price_cents)}</dd></div>
          </dl>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  section { margin-top: 36px; }
  h2 { font-size: 15px; margin-bottom: 10px; }
  ul { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 8px; }
  li {
    display: grid; grid-template-columns: 40px minmax(180px, 1.4fr) minmax(220px, 1.4fr) auto; gap: 16px; align-items: center;
    padding: 10px 16px; background: var(--panel); border: 1px solid var(--line); border-radius: var(--radius);
  }
  img { width: 40px; height: 40px; border-radius: 6px; background: var(--panel-2); object-fit: contain; }
  .who, .st { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .st .pill { align-self: flex-start; }
  .small { font-size: 12px; }
  .amber { color: var(--amber); }
  .sell { display: flex; gap: 20px; margin: 0; }
  .sell div { display: flex; flex-direction: column; }
  dt { color: var(--faint); font-size: 12px; }
  dd { margin: 0; font-weight: 650; }
  .empty { color: var(--muted); padding: 18px 16px; background: var(--panel); border-radius: var(--radius); max-width: 70ch; }
  @media (max-width: 899px) {
    li { grid-template-columns: 40px 1fr; row-gap: 8px; }
    .st, .sell { grid-column: 1 / -1; }
    .sell { justify-content: space-between; }
  }
</style>
