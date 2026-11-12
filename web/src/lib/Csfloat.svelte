<script lang="ts">
  import { app } from './store.svelte';
  import { ago, floatTxt, usd } from './format';
  import type { CsfloatDeal } from './types';

  const KIND: Record<CsfloatDeal['kind'], string> = { pattern: 'Pattern raro', price: 'Abaixo das vendas' };
  const day = $derived(app.csfloat.filter((d) => app.now - d.at < 86400));
  const open = $derived(day.filter((d) => !d.gone_at && d.state));
  const sold = $derived(day.filter((d) => d.state === 'sold'));

  // A conferência é espaçada (1 min, 10 min, 1 h, 6 h, 24 h): o tempo é um teto, não a hora exata em que saiu.
  function lasted(d: CsfloatDeal): string {
    const s = d.gone_at! - d.at;
    return s < 3600 ? `${Math.max(1, Math.round(s / 60))} min` : `${Math.round(s / 3600)} h`;
  }
</script>

<section aria-labelledby="h-csfloat">
  <h2 id="h-csfloat" class="sr-only">Achados no CSFloat</h2>
  <p class="muted sub">
    Anúncios novos do CSFloat abaixo do que já vendeu de verdade. Valores em dólar, lucro já sem a taxa de 2%.
    O programa só avisa: comprar exige saldo no CSFloat.
  </p>
  {#if app.csfloat.length === 0}
    <p class="empty">Nenhum achado com prova ainda. O feed é lido a cada 20 s.</p>
  {:else}
    <p class="sum">
      <b>{day.length}</b> em 24 h · <b>{open.length}</b> ainda à venda · <b>{sold.length}</b> vendidos para outra pessoa
    </p>
    <ul>
      {#each app.csfloat as d (d.id)}
        <li class:gone={!!d.gone_at}>
          <div class="who">
            <a href={'https://csfloat.com/item/' + d.id} target="_blank" rel="noopener noreferrer">{d.name}</a>
            <span class="small muted">
              <span class="pill mute">{KIND[d.kind] ?? d.kind}</span>
              {#if d.seed != null}seed {d.seed}{/if}
              {#if d.float != null}· float {floatTxt(d.float)}{/if}
              · {ago(d.at, app.now)}
            </span>
            <span class="small muted">{d.evidence}</span>
          </div>
          <dl>
            <div><dt>Preço</dt><dd>{usd(d.price_cents)}</dd></div>
            <div><dt>Revenda</dt><dd>{usd(d.sell_cents)}</dd></div>
            <div><dt>Lucro</dt><dd class="up">+{usd(d.profit_cents)}</dd></div>
          </dl>
          <div class="st">
            {#if d.gone_at}
              <span class="pill mute">{d.state === 'sold' ? 'Vendido' : 'Saiu do ar'}</span>
              <span class="small muted">em até {lasted(d)}</span>
            {:else if d.state}
              <span class="pill ok">À venda</span>
            {:else}
              <span class="pill warn">Conferindo…</span>
            {/if}
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .sub { margin: 0 0 12px; max-width: 80ch; }
  .sum { margin: 0 0 12px; }
  ul { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 8px; }
  li {
    display: grid; grid-template-columns: minmax(240px, 2fr) auto minmax(120px, auto); gap: 16px; align-items: center;
    padding: 10px 16px; background: var(--panel); border: 1px solid var(--line); border-radius: var(--radius);
  }
  li.gone { opacity: 0.6; }
  .who, .st { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .st .pill { align-self: flex-start; }
  .who a { font-weight: 650; }
  .small { font-size: 12px; }
  dl { display: flex; gap: 20px; margin: 0; }
  dl div { display: flex; flex-direction: column; }
  dt { color: var(--muted); font-size: 12px; }
  dd { margin: 0; font-weight: 650; }
  .empty { color: var(--muted); padding: 24px 0; }
  @media (max-width: 899px) {
    li { grid-template-columns: 1fr; row-gap: 8px; }
    dl { justify-content: space-between; }
  }
</style>
