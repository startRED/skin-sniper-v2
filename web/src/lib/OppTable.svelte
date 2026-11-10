<script lang="ts">
  import { app, loadHistory, lostRows, marginOf, purchaseOf, rows, setSort, setTier, toggleExpanded, type SortKey } from './store.svelte';
  import { ago, eta, floatTxt, money, pct, signed } from './format';
  import Chart from './Chart.svelte';
  import type { GoneReason, Opportunity } from './types';

  // lost: as que já sumiram da lista, apagadas e sem compra; cabeçalho sem ordenação (no celular, nenhum)
  let { lost = false }: { lost?: boolean } = $props();

  const LOST_SHOWN = 5;
  let showAll = $state(false);
  const lostAll = $derived(lost ? lostRows() : []);

  let daySort = $state<'asc' | 'desc' | null>('desc');
  const list = $derived.by(() => {
    if (lost) return showAll ? lostAll : lostAll.slice(0, LOST_SHOWN);
    const current = rows();
    if (!daySort) return current;
    return current.sort((a, b) => {
      const value = (o: Opportunity) => o.profit_per_day_cents ?? o.sell.mid.profit_cents / 7;
      return (daySort === 'desc' ? -1 : 1) * (value(a) - value(b)) || b.seen_at - a.seen_at;
    });
  });
  const inCombo = $derived(new Set(app.highlight ? (app.state?.combo.ids ?? []) : []));
  const CONF = { alta: 'alta', media: 'média', baixa: 'baixa' } as const;
  const RISK = { baixo: 'baixo', medio: 'médio', alto: 'alto' } as const;
  const STATUS: Record<GoneReason, string> = {
    unavailable: 'Vendido ou retirado', unprofitable: 'Deixou de compensar',
    unwatched: 'Sem acompanhamento', bought: 'Você comprou', rejected: 'Compra recusada'
  };
  const TIERS: [string, 'quick' | 'mid' | 'high', string][] = [
    ['Rápido', 'quick', 'O mais baixo dos três: maior chance de vender logo'],
    ['Médio', 'mid', 'Preço mais provável de venda'],
    ['Alto', 'high', 'Otimista: pode demorar mais para vender']
  ];
  const DAY_HINT = 'Lucro no preço médio dividido pelos dias até vender, contando os 7 de trava. Sem prazo estimado, a ordem usa o lucro ÷ 7.';
  // Perdidas mostram sempre o médio: o seletor de cenário fica só na lista viva.
  const tk = $derived(lost ? 'mid' : app.tier);

  const title = (o: Opportunity) => (o.wear && o.name.endsWith(`(${o.wear})`) ? o.name.slice(0, -(o.wear.length + 3)) : o.name);
  const dirOf = (k: SortKey) => (!daySort && app.sort.key === k ? app.sort.dir : null);
  const aria = (d: 'asc' | 'desc' | null) => (d === 'asc' ? 'ascending' : d === 'desc' ? 'descending' : 'none');
  const confText = (o: Opportunity) => `Confiança ${CONF[o.confidence]}${o.risk ? ` · risco ${RISK[o.risk]}` : ''}`;
  const tiersText = (o: Opportunity) => TIERS.map(([l, k]) => `${l} ${money(o.sell[k].price_cents)} (${signed(o.sell[k].profit_cents)})`).join(' · ');
</script>

{#snippet arrow(dir: 'asc' | 'desc' | null)}
  {#if dir}<svg class="arrow" class:asc={dir === 'asc'} width="10" height="10" viewBox="0 0 16 16" aria-hidden="true"><path d="M3 6l5 5 5-5" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"/></svg>{/if}
{/snippet}

{#snippet sort(key: SortKey, label: string, hint: string)}
  <button onclick={() => { if (daySort && app.sort.key === key) app.sort = { key, dir: key === 'eta' ? 'asc' : 'desc' }; else setSort(key); daySort = null; }} title={hint} class:active={dirOf(key) !== null}>
    {label}{@render arrow(dirOf(key))}
  </button>
{/snippet}

{#if !lost}
  <div class="tools">
    <span id="tier-l">Se eu vender pelo preço</span>
    <div class="seg" role="group" aria-labelledby="tier-l">
      {#each TIERS as [label, key, hint]}
        <button class:active={app.tier === key} aria-pressed={app.tier === key} title={hint} onclick={() => setTier(key)}>{label}</button>
      {/each}
    </div>
  </div>
{/if}
<div class="table" role="table" aria-label={lost ? 'Oportunidades perdidas' : 'Skins com lucro'}>
  <div class="head grid" role="row" class:static={lost}>
    <div class="h hide" role="columnheader">Skin</div>
    <div class="h hide" role="columnheader">Compra</div>
    {#if lost}
      <div class="h" role="columnheader">Lucro</div>
      <div class="h" role="columnheader">Por dia</div>
      <div class="h" role="columnheader">Vende por</div>
      <div class="h" role="columnheader">Prazo</div>
      <div class="h" role="columnheader">Risco</div>
      <div class="h" role="columnheader">Sumiu</div>
    {:else}
      <div class="h" role="columnheader" aria-sort={aria(dirOf('profit') ?? dirOf('margin'))}>
        {@render sort('profit', 'Lucro', 'Ordenar pelo lucro no cenário escolhido')}
        {@render sort('margin', 'Margem', 'Ordenar pela margem: lucro dividido pelo preço')}
      </div>
      <div class="h" role="columnheader" aria-sort={aria(daySort)}>
        <button onclick={() => { daySort = daySort === 'desc' ? 'asc' : 'desc'; }} class:active={daySort !== null} title={DAY_HINT}>Por dia{@render arrow(daySort)}</button>
      </div>
      <div class="h hide" role="columnheader" title="Preço de venda no cenário escolhido; abaixo, do rápido ao alto">Vende por</div>
      <div class="h" role="columnheader" aria-sort={aria(dirOf('eta'))}>{@render sort('eta', 'Prazo', 'Ordenar pelo tempo estimado para vender (menor primeiro)')}</div>
      <div class="h" role="columnheader" aria-sort={aria(dirOf('safe'))}>{@render sort('safe', 'Risco', 'Ordenar pelo lucro que sobra vendendo pelo preço rápido')}</div>
      <div class="h" role="columnheader" aria-sort={aria(dirOf('new'))}>{@render sort('new', 'Apareceu', 'Ordenar pelas mais novas')}</div>
    {/if}
    <div class="h hide" role="columnheader"><span class="sr-only">{lost ? 'Motivo' : 'Abrir na Steam'}</span></div>
  </div>

  {#each list as o (o.id)}
    {@const open = app.expanded === o.id}
    {@const checked = o.checked_at ?? o.seen_at}
    {@const stale = app.now - checked >= 180}
    <div class="item" class:fresh={app.fresh[o.id]} class:combo={inCombo.has(o.id)} class:open class:gone={lost}>
      <!-- A linha inteira abre os detalhes; pelo teclado, o botão da skin faz o mesmo. -->
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_interactive_supports_focus -->
      <div class="row grid" role="row" onclick={(e) => { if (!(e.target as Element).closest('a, button')) toggleExpanded(o.id); }}>
        <div class="c skin" role="cell">
          <button class="skinbtn" aria-expanded={open} aria-controls="d-{o.id}" onclick={() => toggleExpanded(o.id)}
            title={open ? 'Fechar detalhes' : 'Ver gráfico e detalhes'}>
            <svg class="chev" width="14" height="14" viewBox="0 0 16 16" aria-hidden="true"><path d="M4 6l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>
            <img src={o.icon_url} alt="" width="44" height="44" loading="lazy" />
            <span class="names">
              <span class="nm">{title(o)}</span>
              <span class="meta">
                {#if o.wear}<span>{o.wear}</span>{/if}
                {#if o.float != null}<span>float {floatTxt(o.float)}</span>{/if}
                {#if o.seed != null}<span>seed {o.seed}</span>{/if}
                {#if o.group}<span class="pill {o.kind === 'raro' ? 'warn' : 'mute'}">{o.group}</span>{/if}
                {#if o.seal}<span class="pill {o.seal.proven ? 'ok' : 'mute'}" title={o.seal.text}>{o.seal.proven ? 'comprovado' : 'não comprovado'}</span>{/if}
              </span>
            </span>
          </button>
        </div>
        <div class="c price" role="cell" data-l="Compra"><b>{money(o.price_cents)}</b></div>
        <div class="c profit" role="cell" data-l="Lucro">
          <b class={o.sell[tk].profit_cents > 0 ? 'up' : 'down'}>{signed(o.sell[tk].profit_cents)}</b>
          <small class="muted">{pct(lost ? o.margin_pct : marginOf(o))}</small>
        </div>
        <div class="c perday" role="cell" data-l="Por dia" title={DAY_HINT}>{o.profit_per_day_cents == null ? '—' : signed(o.profit_per_day_cents)}</div>
        <div class="c sell" role="cell" data-l="Vende por" title={tiersText(o)}>
          <b>{money(o.sell[tk].price_cents)}</b>
          <small class="muted">{money(o.sell.quick.price_cents)} a {money(o.sell.high.price_cents)}</small>
        </div>
        <div class="c eta" role="cell" data-l="Prazo">{eta(o.sell[tk].eta_hours)}</div>
        <div class="c risk" role="cell" data-l="Risco">
          <span class="pill {o.safe ? 'ok' : 'warn'}" title={o.safe ? 'Dá lucro até vendendo pelo preço rápido' : 'Vendendo pelo preço rápido você perde dinheiro'}>{o.safe ? 'Seguro' : 'Arriscado'}</span>
          <button type="button" class="cf {o.risk === 'alto' ? 'baixa' : o.risk === 'medio' ? 'media' : o.confidence}" data-tip={o.evidence} aria-label="{confText(o)}: {o.evidence}">{confText(o)}</button>
        </div>
        {#if lost}
          <div class="c seen muted" role="cell" data-l="Sumiu">{ago(o.gone_at!, app.now)}</div>
          <div class="c act" role="cell">
            {#if purchaseOf(o.id)?.status === 'bought'}
              <span class="pill ok">Você comprou</span>
            {:else}
              <span class="pill mute" title={o.gone_reason === 'unavailable' ? 'A Steam não devolveu mais este anúncio; não dá para distinguir venda de retirada.' : undefined}>{o.gone_reason ? STATUS[o.gone_reason] : 'Indisponível'}</span>
            {/if}
          </div>
        {:else}
          <div class="c seen" role="cell" data-l="Apareceu">
            <span>{ago(o.seen_at, app.now)}</span>
            <small class={stale ? 'stale' : 'muted'} title="Quando a Steam devolveu este anúncio pela última vez">{stale ? 'sem conferir' : 'conferido'} {ago(checked, app.now)}</small>
          </div>
          <div class="c act" role="cell"><a class="btn primary" href={o.steam_url} target="_blank" rel="noopener noreferrer" aria-label="Abrir {o.name} na Steam">Abrir na Steam</a></div>
        {/if}
      </div>

      {#if open}
        {@const h = app.history[o.id]}
        <div class="detail" id="d-{o.id}">
          <div class="chart">
            <h3>Preço nos últimos 30 dias</h3>
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
            {#if o.seal}<p class="ev"><b>Raro {o.seal.proven ? 'comprovado' : 'ainda não comprovado'}:</b> {o.seal.text.replace(/^(ainda não )?comprovado: /, '')}{#if o.seal.open_cents != null && o.seal.floor_cents != null && o.seal.open_cents > o.seal.floor_cents}. Anuncie a {money(o.seal.open_cents)} e desça até {money(o.seal.floor_cents)}.{/if}</p>{/if}
            <div class="tbl" role="table" aria-label="Preços para vender {o.name}">
              <div class="tr th" role="row"><span role="columnheader">Vender por</span><span role="columnheader">Você recebe</span><span role="columnheader">Lucro</span><span role="columnheader">Prazo</span></div>
              {#each TIERS as [label, key, hint]}
                {@const t = o.sell[key]}
                <div class="tr" class:sel={key === tk} role="row" title={hint}>
                  <span role="cell"><em>{label}</em> {money(t.price_cents)}</span>
                  <span role="cell">{money(t.net_cents)}</span>
                  <span role="cell" class={t.profit_cents > 0 ? 'up' : 'down'}>{signed(t.profit_cents)}</span>
                  <span role="cell">{eta(t.eta_hours)}</span>
                </div>
              {/each}
            </div>
            <p class="muted small">Prazos contados depois dos 7 dias de trava.</p>
            {#if lost || o.inspect_url}
              <div class="links">
                {#if lost}<a class="btn" href={o.steam_url} target="_blank" rel="noopener noreferrer">Abrir na Steam</a>{/if}
                {#if o.inspect_url}<a class="btn" href={o.inspect_url}>Inspecionar no jogo</a>{/if}
              </div>
            {/if}
          </div>
        </div>
      {/if}
    </div>
  {/each}
</div>
{#if lostAll.length > LOST_SHOWN}
  <button class="btn more" onclick={() => (showAll = !showAll)}>{showAll ? 'Mostrar só as mais recentes' : `Mostrar todas as ${lostAll.length}`}</button>
{/if}

<style>
  .table { display: flex; flex-direction: column; }
  .tools { display: flex; align-items: center; justify-content: flex-end; gap: 10px; color: var(--muted); font-size: 13px; }
  .seg { display: flex; gap: 3px; padding: 3px; background: var(--panel-2); border-radius: 7px; }
  .seg button { border: 0; background: transparent; color: var(--muted); padding: 0 12px; min-height: 30px; border-radius: 5px; font-size: 13px; }
  .seg button:hover { color: var(--ink); }
  .seg button.active { background: #303a43; color: var(--ink); font-weight: 650; }
  .grid {
    display: grid; align-items: center; column-gap: 12px; padding: 0 12px;
    grid-template-columns:
      minmax(240px, 3.6fr) minmax(84px, 0.8fr) minmax(110px, 1fr) minmax(84px, 0.8fr) minmax(130px, 1.2fr)
      minmax(70px, 0.6fr) minmax(170px, 1.4fr) minmax(128px, 1.1fr) 128px;
  }
  .head { position: sticky; top: var(--top); z-index: 5; background: var(--bg); min-height: 36px; border-bottom: 1px solid var(--line); color: var(--muted); font-size: 12px; }
  .head.static { position: static; }
  .h { display: flex; gap: 10px; }
  .h button { display: inline-flex; align-items: center; gap: 4px; background: none; border: 0; padding: 6px 0; color: inherit; font-size: 12px; }
  .h button:hover, .h button.active { color: var(--ink); }
  .arrow { color: var(--green); }
  .arrow.asc { transform: rotate(180deg); }

  .item { border-bottom: 1px solid var(--line); position: relative; }
  .item.gone { color: var(--muted); }
  .item.gone img { filter: grayscale(0.8); opacity: 0.7; }
  .item:hover { background: #12171c; }
  .item.open { background: var(--panel); }
  .item.combo { background: var(--green-bg); box-shadow: inset 0 0 0 1px rgba(62, 207, 142, 0.45); }
  .item.fresh { animation: flash 3.4s ease-out; }
  @keyframes flash { 0%, 25% { background: rgba(62, 207, 142, 0.28); } 100% { background: transparent; } }

  .row { min-height: 60px; padding-top: 8px; padding-bottom: 8px; cursor: pointer; }
  .c { min-width: 0; display: flex; flex-direction: column; justify-content: center; gap: 2px; }
  .c b { font-weight: 650; }
  .c small { font-size: 12px; white-space: nowrap; }
  .c::before { content: attr(data-l); display: none; }
  .stale { color: var(--amber); }

  .skinbtn { display: flex; align-items: center; gap: 10px; width: 100%; background: none; border: 0; padding: 0; text-align: left; min-width: 0; }
  .skinbtn img { width: 44px; height: 44px; border-radius: 6px; background: var(--panel-2); flex: none; object-fit: contain; }
  .names { display: flex; flex-direction: column; gap: 3px; min-width: 0; flex: 1; }
  .nm { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .meta { display: flex; flex-wrap: wrap; gap: 2px 10px; color: var(--muted); font-size: 12px; align-items: center; }
  .chev { color: var(--faint); flex: none; transform: rotate(-90deg); transition: transform 0.15s; }
  .row:hover .chev { color: var(--ink); }
  .open .chev { transform: none; }

  .profit b { font-size: 16px; }
  .risk { align-items: flex-start; gap: 3px; }

  .cf { border: 0; padding: 0; background: none; font-size: 12px; cursor: help; position: relative; white-space: nowrap; color: var(--muted); text-decoration: underline dotted; text-underline-offset: 3px; }
  .cf.baixa { color: var(--amber); }
  .cf::after {
    content: attr(data-tip); position: absolute; right: 0; top: calc(100% + 6px); width: 260px; z-index: 20;
    background: #222a33; color: var(--ink); border: 1px solid #323c47; border-radius: 6px; padding: 8px 10px;
    font-weight: 400; font-size: 12px; line-height: 1.35; white-space: normal; text-align: left; display: none; box-shadow: 0 6px 20px rgba(0, 0, 0, 0.5);
  }
  .cf:hover::after, .cf:focus-visible::after { display: block; }

  .chart, .info { min-width: 0; }
  .detail { display: grid; grid-template-columns: minmax(0, 1.7fr) minmax(300px, 1fr); gap: 28px; padding: 12px 12px 20px 36px; }
  .detail h3 { font-size: 13px; font-weight: 600; color: var(--muted); margin-bottom: 6px; }
  .note { padding: 40px 0; text-align: center; }
  .info { display: flex; flex-direction: column; gap: 12px; }
  .ev { line-height: 1.45; }
  .tbl { display: flex; flex-direction: column; font-size: 13px; }
  .tr { display: grid; grid-template-columns: 1.5fr 1fr 1fr 0.9fr; gap: 8px; padding: 6px 0; border-bottom: 1px solid var(--line); }
  .tr.th { color: var(--muted); font-size: 12px; }
  .tr.sel { color: var(--ink); font-weight: 650; }
  .tr em { font-style: normal; color: var(--muted); margin-right: 4px; }
  .small { font-size: 12px; }
  .links { display: flex; gap: 8px; flex-wrap: wrap; }
  .act .pill { width: fit-content; }
  .more { margin-top: 10px; align-self: flex-start; }

  @media (max-width: 1279px) {
    .tools { justify-content: flex-start; flex-wrap: wrap; }
    .seg button { min-height: 36px; }
    .head { position: static; border: 0; padding: 0 0 8px; display: flex; flex-wrap: nowrap; overflow-x: auto; scrollbar-width: none; gap: 6px; min-height: 0; }
    .head .hide, .head.static { display: none; }
    .h { gap: 6px; }
    .h button { white-space: nowrap; border: 1px solid var(--line); border-radius: 99px; padding: 0 12px; min-height: 36px; font-size: 13px; background: var(--panel); }
    .h button.active { border-color: var(--green); color: var(--green); }
    .table { gap: 10px; }
    .item { border: 1px solid var(--line); border-radius: var(--radius); background: var(--panel); overflow: hidden; }
    .row { grid-template-columns: repeat(12, 1fr); gap: 12px 8px; padding: 14px; align-items: start; }
    .c::before { display: block; color: var(--faint); font-size: 11px; }
    .c.skin { grid-column: span 12; }
    .c.price, .c.profit, .c.perday, .c.seen { grid-column: span 4; }
    .c.sell { grid-column: span 5; }
    .c.eta { grid-column: span 3; }
    .c.risk { grid-column: span 12; flex-direction: row; flex-wrap: wrap; justify-content: flex-start; align-items: center; gap: 4px 10px; }
    .c.risk::before { width: 100%; }
    .c.risk { order: 1; } /* depois de "apareceu", que fecha a linha de venda e prazo */
    .c.act { grid-column: span 12; order: 2; }
    .c.act::before, .c.skin::before { display: none; }
    .c.act .btn { min-height: 44px; }
    .nm { white-space: normal; }
    .detail { grid-template-columns: minmax(0, 1fr); padding: 4px 14px 16px; gap: 16px; }
    .c.risk, .cf { position: static; }
    .cf::after { left: 14px; right: 14px; width: auto; top: auto; bottom: 84px; }
  }
</style>
