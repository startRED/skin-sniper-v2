<script lang="ts">
  import { app, isUncertain } from './store.svelte';
  import { clock, day, money, until, parseBRL, pct } from './format';
  import { api } from './api';
  import type { Purchase } from './types';

  let amounts = $state<Record<string, string>>({});
  let errors = $state<Record<string, string>>({});
  let saving = $state<Record<string, boolean>>({});

  async function save(p: Purchase) {
    const cents = parseBRL(amounts[p.id] ?? '');
    if (cents === null || !Number.isSafeInteger(cents) || cents <= 0) {
      errors[p.id] = 'Informe um valor válido, maior que zero.';
      return;
    }
    saving[p.id] = true;
    errors[p.id] = '';
    try {
      const updated = await api<Purchase>(`/api/purchases/${encodeURIComponent(p.id)}/sold`, 'POST', { sold_cents: cents });
      const i = app.purchases.findIndex((item) => item.id === p.id);
      if (i >= 0) app.purchases[i] = updated;
    } catch (e) {
      errors[p.id] = e instanceof Error ? e.message : 'Não consegui salvar a venda.';
    } finally { saving[p.id] = false; }
  }

  function comparison(sold: number, predicted: number): string {
    if (predicted <= 0) return 'sem previsão';
    const difference = (sold - predicted) * 100 / predicted;
    return Math.abs(difference) < 0.05 ? 'igual' : `${pct(Math.abs(difference))} ${difference > 0 ? 'acima' : 'abaixo'}`;
  }

  const STATUS: Record<Purchase['status'], [string, string]> = {
    pending: ['warn', 'Pendente: confira na Steam'],
    need_confirmation: ['warn', 'Confira na Steam'],
    bought: ['ok', 'Comprada'],
    failed: ['bad', 'Não comprou']
  };
</script>

<section aria-labelledby="h-compras">
  <h2 id="h-compras" class="sr-only">Compras</h2>
  {#if app.purchases.length === 0}
    <p class="empty">Nenhuma compra registrada. Compras feitas direto na Steam não aparecem aqui.</p>
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
          {#if p.status === 'bought' && p.tradable_at && p.tradable_at <= app.now}
            <div class="sold">
              {#if p.sold_cents != null}
                <span>Vendeu por {money(p.sold_cents)}</span>
                <span class="small muted">Rápido: {comparison(p.sold_cents, p.sell.quick.price_cents)} · Médio: {comparison(p.sold_cents, p.sell.mid.price_cents)} · Alto: {comparison(p.sold_cents, p.sell.high.price_cents)}</span>
              {/if}
              <form onsubmit={(event) => { event.preventDefault(); save(p); }}>
                <label for={'sold-' + p.id}>Vendi por R$</label>
                <input id={'sold-' + p.id} inputmode="decimal" placeholder="0,00" bind:value={amounts[p.id]} />
                <button type="submit" disabled={saving[p.id]}>{saving[p.id] ? 'Salvando…' : 'Salvar venda'}</button>
              </form>
              <span class="small muted">O preço que o comprador pagou, com as taxas.</span>
              {#if errors[p.id]}<span class="small" role="alert">{errors[p.id]}</span>{/if}
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
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
  .sold { grid-column: 2 / -1; display: flex; flex-direction: column; gap: 4px; }
  .sold form { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; }
  .sold input { width: 100px; }
  dt { color: var(--muted); font-size: 12px; }
  .sold label { color: var(--muted); }
  .sold input { min-height: 36px; padding: 0 10px; border-radius: var(--radius); background: var(--bg); border: 1px solid var(--line); }
  .sold button { min-height: 36px; padding: 0 14px; border-radius: var(--radius); border: 1px solid var(--line); background: var(--panel-2); }
  .sold [role=alert] { color: var(--red); }
  dd { margin: 0; font-weight: 650; }
  .empty { color: var(--muted); padding: 24px 0; }
  @media (max-width: 899px) {
    li { grid-template-columns: 40px 1fr; row-gap: 8px; }
    .st, .sell, .sold { grid-column: 1 / -1; }
    .sell { justify-content: space-between; }
  }
</style>
