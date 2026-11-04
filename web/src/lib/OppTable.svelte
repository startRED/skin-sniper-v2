<script lang="ts">
  import { app, loadHistory, rows, setSort, toggleExpanded, type SortKey } from './store.svelte';
  import { ago, eta, floatTxt, money, pct, signed } from './format';
  import BuyButton from './BuyButton.svelte';
  import Chart from './Chart.svelte';
  import type { Opportunity, Tier } from './types';

  let daySort = $state<'asc' | 'desc' | null>('desc');
  const list = $derived.by(() => {
    const current = rows();
    if (!daySort) return current;
    return current.sort((a, b) => {
      const value = (o: Opportunity) => o.profit_per_day_cents ?? o.sell.mid.profit_cents / 7;
      return (daySort === 'desc' ? -1 : 1) * (value(a) - value(b)) || b.seen_at - a.seen_at;
    });
  });
  const inCombo = $derived(new Set(app.highlight ? (app.state?.combo.ids ?? []) : []));
  const CONF = { alta: 'Alta', media: 'Média', baixa: 'Baixa' } as const;
  const RISK = { baixo: 'baixo', medio: 'médio', alto: 'alto' } as const;
  const TIERS: [string, 'quick' | 'mid' | 'high', string][] = [
    ['Rápido', 'quick', 'Vendendo rápido você ainda ganha: o preço é o mais baixo dos três, o de maior chance de vender logo'],
    ['Médio', 'mid', 'Preço mais provável de venda'],
    ['Alto', 'high', 'Preço otimista: pode demorar mais para vender']
  ];

  const title = (o: Opportunity) => (o.wear && o.name.endsWith(`(${o.wear})`) ? o.name.slice(0, -(o.wear.length + 3)) : o.name);
  const sortAttr = (k: SortKey) => (!daySort && app.sort.key === k ? (app.sort.dir === 'asc' ? 'ascending' : 'descending') : 'none');
</script>

{#snippet th(key: SortKey, label: string, hint: string)}
  <div class="h sortable" role="columnheader" aria-sort={sortAttr(key)}>
    <button onclick={() => { if (daySort && app.sort.key === key) app.sort = { key, dir: key === 'eta' ? 'asc' : 'desc' }; else setSort(key); daySort = null; }} title={hint} class:active={!daySort && app.sort.key === key}>
      {label}<span class="arrow" aria-hidden="true">{!daySort && app.sort.key === key ? (app.sort.dir === 'asc' ? '▲' : '▼') : ''}</span>
    </button>
  </div>
{/snippet}

{#snippet tier(t: Tier, showProfit: boolean)}
  <b>{money(t.price_cents)}</b>
  {#if showProfit}<small class={t.profit_cents > 0 ? 'up' : 'down'}>{signed(t.profit_cents)}</small>{/if}
{/snippet}

<p class="day-hint">Por dia: lucro estimado dividido pelo tempo até vender, incluindo os 7 dias de trava. Sem esse dado, a ordem usa o lucro médio dividido por 7, apenas para comparação.</p>
<div class="table" role="table" aria-label="Skins com lucro">
  <div class="head grid" role="row">
    <div class="h hide" role="columnheader">Skin</div>
    <div class="h hide" role="columnheader">Preço</div>
    <div class="h sortable" role="columnheader" aria-sort={daySort ? (daySort === 'asc' ? 'ascending' : 'descending') : 'none'}>
      <button onclick={() => { daySort = daySort === 'desc' ? 'asc' : 'desc'; }} class:active={daySort !== null} title="Lucro estimado dividido pelos dias até vender, incluindo os 7 dias de trava. É uma estimativa, não uma garantia.">Por dia<span class="arrow" aria-hidden="true">{daySort ? (daySort === 'asc' ? '▲' : '▼') : ''}</span></button>
    </div>
    {@render th('profit', 'Lucro', 'Ordenar pelo lucro no preço médio')}
    {@render th('margin', 'Margem', 'Ordenar pela margem: lucro dividido pelo preço')}
    {@render th('safe', 'Venda rápida', 'Ordenar pelo lucro seguro: o que sobra vendendo pelo preço rápido')}
    <div class="h hide" role="columnheader">Venda média</div>
    <div class="h hide" role="columnheader">Venda alta</div>
    {@render th('eta', 'Prazo', 'Ordenar pelo tempo estimado para vender (menor primeiro)')}
    <div class="h hide" role="columnheader">Confiança</div>
    {@render th('new', 'Mais novo', 'Ordenar pelas que apareceram agora')}
    <div class="h hide" role="columnheader"><span class="sr-only">Comprar</span></div>
  </div>

  {#each list as o (o.id)}
    {@const open = app.expanded === o.id}
    <div class="item" class:fresh={app.fresh[o.id]} class:combo={inCombo.has(o.id)} class:open class:risky={!o.safe}>
      <div class="row grid" role="row">
        <div class="c skin" role="cell">
          <button class="skinbtn" aria-expanded={open} aria-controls="d-{o.id}" onclick={() => toggleExpanded(o.id)}
            title={open ? 'Fechar detalhes' : 'Ver gráfico e detalhes'}>
            <img src={o.icon_url} alt="" width="48" height="48" loading="lazy" />
            <span class="names">
              <span class="nm">{title(o)}</span>
              <span class="meta">
                {#if o.wear}<span>{o.wear}</span>{/if}
                {#if o.float != null}<span>float {floatTxt(o.float)}</span>{/if}
                {#if o.seed != null}<span>seed {o.seed}</span>{/if}
                {#if o.group}<span class="pill {o.kind === 'raro' ? 'warn' : 'mute'}">{o.group}</span>{/if}
              </span>
            </span>
            <svg class="chev" width="14" height="14" viewBox="0 0 16 16" aria-hidden="true"><path d="M4 6l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>
          </button>
        </div>
        <div class="c price" role="cell" data-l="Preço"><b>{money(o.price_cents)}</b></div>
        <div class="c perday" role="cell" data-l="Por dia"><b>{o.profit_per_day_cents == null ? 'Sem dados' : signed(o.profit_per_day_cents)}</b></div>
        <div class="c profit" role="cell" data-l="Lucro">
          <b class={o.sell.mid.profit_cents > 0 ? 'up' : 'down'}>{signed(o.sell.mid.profit_cents)}</b>
          {#if o.safe}
            <span class="pill ok" title="Dá lucro até vendendo pelo preço rápido">✓ Seguro</span>
          {:else}
            <span class="pill warn" title="Vendendo pelo preço rápido você perde dinheiro">Arriscado</span>
          {/if}
        </div>
        <div class="c margin" role="cell" data-l="Margem">{pct(o.margin_pct)}</div>
        <div class="c tier" role="cell" data-l="Venda rápida">{@render tier(o.sell.quick, true)}</div>
        <div class="c tier" role="cell" data-l="Venda média">{@render tier(o.sell.mid, false)}</div>
        <div class="c tier" role="cell" data-l="Venda alta">{@render tier(o.sell.high, true)}</div>
        <div class="c eta" role="cell" data-l="Prazo">{eta(o.sell.mid.eta_hours)}</div>
        <div class="c conf" role="cell" data-l="Confiança">
          <button type="button" class="cf {o.risk === 'alto' ? 'baixa' : o.risk === 'medio' ? 'media' : o.confidence}" data-tip={o.evidence} aria-label="Confiança {CONF[o.confidence]}{o.risk ? `, risco ${RISK[o.risk]}` : ''}: {o.evidence}">{CONF[o.confidence]}{o.risk ? ` · risco ${RISK[o.risk]}` : ''}</button>
        </div>
        <div class="c seen muted" role="cell" data-l="Apareceu">{ago(o.seen_at, app.now)}</div>
        <div class="c act" role="cell"><BuyButton {o} /></div>
      </div>

      {#if open}
        {@const h = app.history[o.id]}
        <div class="detail" id="d-{o.id}">
          <div class="chart">
            <h3>Preço dos últimos 30 dias</h3>
            {#if !h || h === 'loading'}
              <p class="muted note">Carregando o gráfico…</p>
            {:else if h === 'error'}
              <p class="note">Não consegui carregar o gráfico. <button class="btn" onclick={() => loadHistory(o.id)}>Tentar de novo</button></p>
            {:else}
              <Chart history={h} paid={o.price_cents} now={app.now} />
            {/if}
          </div>
          <div class="info">
            <p class="ev"><b>Por que confiar:</b> {o.evidence}</p>
            <div class="tbl" role="table" aria-label="Preços para vender {o.name}">
              <div class="tr th" role="row"><span role="columnheader">Vender por</span><span role="columnheader">Você recebe</span><span role="columnheader">Lucro</span><span role="columnheader">Prazo</span></div>
              {#each TIERS as [label, key, hint]}
                {@const t = o.sell[key]}
                <div class="tr" role="row" title={hint}>
                  <span role="cell"><em>{label}</em> {money(t.price_cents)}</span>
                  <span role="cell">{money(t.net_cents)}</span>
                  <span role="cell" class={t.profit_cents > 0 ? 'up' : 'down'}>{signed(t.profit_cents)}</span>
                  <span role="cell">{eta(t.eta_hours)}</span>
                </div>
              {/each}
            </div>
            <p class="muted small">A skin comprada fica 7 dias travada: os preços acima valem para depois disso.</p>
            <div class="links">
              <a class="btn" href={o.steam_url} target="_blank" rel="noopener noreferrer">Abrir na Steam</a>
              {#if o.inspect_url}
                <a class="btn" href={o.inspect_url}>Inspecionar no jogo</a>
              {:else}
                <span class="btn" aria-disabled="true" style="opacity:.45" title="Esta skin não trouxe link de inspeção">Inspecionar no jogo</span>
              {/if}
            </div>
          </div>
        </div>
      {/if}
    </div>
  {/each}
</div>

<style>
  .day-hint { color: var(--muted); font-size: 12px; margin: 0 16px 6px; }
  .table { display: flex; flex-direction: column; gap: 0; }
  .grid {
    display: grid; align-items: center; column-gap: 12px; padding: 0 16px;
    grid-template-columns:
      minmax(240px, 2.6fr) minmax(80px, 1fr) minmax(80px, 1fr) minmax(104px, 1.2fr) minmax(64px, 0.8fr)
      repeat(3, minmax(88px, 1fr)) minmax(72px, 0.8fr) minmax(76px, 0.9fr) minmax(66px, 0.8fr) minmax(128px, 1.3fr);
  }
  .head { position: sticky; top: 61px; z-index: 5; background: var(--bg); min-height: 38px; border-bottom: 1px solid var(--line); color: var(--muted); font-size: 12px; }
  .h { padding: 6px 0; line-height: 1.15; }
  .h button { background: none; border: 0; padding: 4px 0; color: inherit; text-align: left; font-size: 12px; line-height: 1.15; }
  .h button:hover, .h button.active { color: var(--ink); }
  .arrow { display: inline-block; width: 12px; margin-left: 3px; font-size: 9px; color: var(--green); }

  .item { border-bottom: 1px solid var(--line); position: relative; }
  .item::before { content: ''; position: absolute; left: 0; top: 0; bottom: 0; width: 3px; background: var(--green); }
  .item.risky::before { background: var(--amber); opacity: 0.7; }
  .item:hover { background: #12171c; }
  .item.open { background: var(--panel); }
  .item.combo { background: var(--green-bg); box-shadow: inset 0 0 0 1px rgba(62, 207, 142, 0.45); }
  .item.fresh { animation: flash 3.4s ease-out; }
  @keyframes flash { 0%, 25% { background: rgba(62, 207, 142, 0.28); } 100% { background: transparent; } }

  .row { min-height: 68px; padding-top: 8px; padding-bottom: 8px; }
  .c { min-width: 0; display: flex; flex-direction: column; justify-content: center; gap: 2px; }
  .c b { font-weight: 650; }
  .c small { font-size: 12px; font-weight: 600; }
  .c::before { content: attr(data-l); display: none; }

  .skinbtn { display: flex; align-items: center; gap: 12px; width: 100%; background: none; border: 0; padding: 0; text-align: left; min-width: 0; }
  .skinbtn img { width: 48px; height: 48px; border-radius: 6px; background: var(--panel-2); flex: none; object-fit: contain; }
  .names { display: flex; flex-direction: column; gap: 3px; min-width: 0; flex: 1; }
  .nm { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .meta { display: flex; flex-wrap: wrap; gap: 2px 10px; color: var(--muted); font-size: 12px; align-items: center; }
  .chev { color: var(--faint); flex: none; transition: transform 0.15s; }
  .open .chev { transform: rotate(180deg); }

  .profit b { font-size: 17px; }
  .profit .pill { align-self: flex-start; }
  .price b { font-size: 15px; }

  .cf { border: 0; padding: 1px 8px; border-radius: 99px; font-size: 12px; font-weight: 600; cursor: help; position: relative; width: fit-content; background: var(--panel-2); }
  .cf.alta { color: var(--green); background: var(--green-bg); }
  .cf.media { color: var(--amber); background: var(--amber-bg); }
  .cf.baixa { color: var(--red); background: var(--red-bg); }
  .cf::after {
    content: attr(data-tip); position: absolute; right: 0; top: calc(100% + 6px); width: 240px; z-index: 20;
    background: #222a33; color: var(--ink); border: 1px solid #323c47; border-radius: 6px; padding: 8px 10px;
    font-weight: 400; font-size: 12px; line-height: 1.35; white-space: normal; display: none; box-shadow: 0 6px 20px rgba(0, 0, 0, 0.5);
  }
  .cf:hover::after, .cf:focus-visible::after { display: block; }

  .chart, .info { min-width: 0; }
  .detail { display: grid; grid-template-columns: minmax(0, 1.7fr) minmax(300px, 1fr); gap: 28px; padding: 14px 16px 20px 80px; }
  .detail h3 { font-size: 13px; font-weight: 600; color: var(--muted); margin-bottom: 6px; }
  .note { padding: 40px 0; text-align: center; }
  .info { display: flex; flex-direction: column; gap: 12px; }
  .ev { line-height: 1.45; }
  .tbl { display: flex; flex-direction: column; font-size: 13px; }
  .tr { display: grid; grid-template-columns: 1.5fr 1fr 1fr 0.9fr; gap: 8px; padding: 6px 0; border-bottom: 1px solid var(--line); }
  .tr.th { color: var(--faint); font-size: 12px; }
  .tr em { font-style: normal; color: var(--muted); margin-right: 4px; }
  .small { font-size: 12px; }
  .links { display: flex; gap: 8px; flex-wrap: wrap; }

  @media (max-width: 1239px) {
    .head { top: 0; position: static; border: 0; padding: 0 0 8px; display: flex; flex-wrap: nowrap; overflow-x: auto; gap: 6px; min-height: 0; }
    .h button { white-space: nowrap; }
    .head .hide { display: none; }
    .h { padding: 0; }
    .h button { border: 1px solid var(--line); border-radius: 99px; padding: 6px 12px; font-size: 13px; background: var(--panel); }
    .h button.active { border-color: var(--green); color: var(--green); }
    .table { gap: 10px; }
    .item { border: 1px solid var(--line); border-radius: var(--radius); background: var(--panel); overflow: hidden; }
    .item::before { width: 4px; }
    .row {
      grid-template-columns: repeat(12, 1fr); row-gap: 12px; column-gap: 8px; padding: 14px 14px 14px 18px;
    }
    .c { order: 20; }
    .c::before { display: block; color: var(--faint); font-size: 11px; }
    .c.skin { grid-column: span 12; order: 1; }
    .c.price { grid-column: span 4; order: 2; }
    .c.perday { grid-column: span 4; order: 4; }
    .c.profit { grid-column: span 5; order: 3; }
    .c.margin { grid-column: span 3; order: 4; }
    .c.tier { grid-column: span 4; order: 6; }
    .c.eta { grid-column: span 4; order: 5; }
    .c.conf { grid-column: span 4; order: 9; }
    .c.seen { grid-column: span 4; order: 10; }
    .c.act { grid-column: span 12; order: 11; }
    .c.act::before { display: none; }
    .c.skin::before { display: none; }
    .nm { white-space: normal; }
    .detail { grid-template-columns: minmax(0, 1fr); padding: 4px 14px 16px 18px; gap: 16px; }
    .row { align-items: start; }
    .c.conf, .cf { position: static; }
    .cf::after { left: 18px; right: 14px; width: auto; top: auto; bottom: 84px; }
  }
</style>
