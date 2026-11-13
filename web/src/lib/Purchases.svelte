<script lang="ts">
  import { app, isUncertain, loadHistory, unmarkBought } from './store.svelte';
  import { clock, day, floatTxt, money, signed, until, parseBRL, pct } from './format';
  import { api } from './api';
  import Chart from './Chart.svelte';
  import type { Purchase } from './types';

  let charts = $state<Record<string, boolean>>({});

  function toggleChart(id: string) {
    charts[id] = !charts[id];
    if (charts[id] && !app.history[id]) void loadHistory(id);
  }

  async function undo(p: Purchase) {
    if (!confirm(`Tirar ${p.name} das compras?`)) return;
    try {
      await unmarkBought(p.id);
    } catch (e) {
      errors[p.id] = e instanceof Error ? e.message : 'Não consegui tirar a compra.';
    }
  }

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
    <p class="empty">Nenhuma compra registrada. Comprou uma oportunidade na Steam? Aperte "Comprei" na linha dela.</p>
  {:else}
    <ul>
      {#each app.purchases as p (p.id)}
        {@const v = p.now ?? p.sell}
        <li>
          <img src={p.icon_url} alt="" width="40" height="40" loading="lazy" />
          <div class="who">
            <b>{p.name}</b>
            <span class="small muted">
              {#if p.float != null}float {floatTxt(p.float)}{/if}
              {#if p.seed != null} · seed {p.seed}{/if}
              {#if p.group} · {p.group}{/if}
            </span>
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
          <div class="val">
            <dl class="sell" aria-label={p.now ? 'Preços para vender hoje' : 'Preços para vender previstos na compra'}>
              <div><dt>Rápido</dt><dd>{money(v.quick.price_cents)}</dd></div>
              <div><dt>Médio</dt><dd>{money(v.mid.price_cents)}</dd></div>
              <div><dt>Alto</dt><dd>{money(v.high.price_cents)}</dd></div>
            </dl>
            {#if p.now}
              <span class="small muted">Hoje: lucro <b class={p.now.mid.profit_cents > 0 ? 'up' : 'down'}>{signed(p.now.mid.profit_cents)}</b> no médio, já sem a taxa · na compra o médio era {money(p.sell.mid.price_cents)}</span>
            {:else}
              <span class="small muted">Previsto na compra{p.sold_cents == null ? '; sem estimativa nova hoje' : ''}</span>
            {/if}
          </div>
          {#if p.status === 'bought'}
            <div class="more">
              <button class="btn" aria-expanded={!!charts[p.id]} onclick={() => toggleChart(p.id)}>{charts[p.id] ? 'Fechar gráfico' : 'Ver preço em 30 dias'}</button>
              {#if p.sold_cents == null}<button class="btn" onclick={() => undo(p)} title="Registrou por engano? Tira esta compra da lista.">Não comprei</button>{/if}
              {#if errors[p.id] && !(p.tradable_at && p.tradable_at <= app.now)}<span class="small" role="alert">{errors[p.id]}</span>{/if}
            </div>
            {#if charts[p.id]}
              {@const h = app.history[p.id]}
              <div class="chart">
                {#if !h || h === 'loading'}
                  <p class="muted small">Carregando o gráfico…</p>
                {:else if h === 'error'}
                  <p class="small">Não consegui carregar o gráfico. <button class="btn" onclick={() => loadHistory(p.id)}>Tentar de novo</button></p>
                {:else}
                  <Chart history={h} paid={p.price_cents} now={app.now} />
                  <p class="small muted">Preço comum do item na Steam; as linhas são a previsão feita na compra.</p>
                {/if}
              </div>
            {/if}
          {/if}
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
  .val { display: flex; flex-direction: column; gap: 4px; }
  .more { grid-column: 2 / -1; display: flex; flex-wrap: wrap; gap: 8px; align-items: center; }
  .more .btn { min-height: 32px; font-size: 13px; }
  .more [role=alert] { color: var(--red); }
  .chart { grid-column: 1 / -1; min-width: 0; }
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
    .st, .val, .more, .sold { grid-column: 1 / -1; }
    .sell { justify-content: space-between; }
  }
</style>
