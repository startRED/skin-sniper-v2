<script lang="ts">
  import type { Learning } from './types';
  import { signed } from './format';

  let { data }: { data: Learning } = $props();
  let period = $state(30);
  let chart = $state<'price' | 'finds'>('price');
  let hover = $state('');
  const days = $derived(data.days.slice(0, period).reverse());
  const queries = $derived(days.reduce((n, d) => n + d.queries, 0));
  const finds = $derived(days.reduce((n, d) => n + (d.queries ? d.finds : 0), 0));
  const samples = $derived(days.reduce((n, d) => n + (d.price_samples ?? 0), 0));
  const rate = $derived(queries ? finds / queries * 100 : null);
  const points = $derived(days.map((d, i) => ({
    day: d.day, samples: d.price_samples ?? 0,
    value: chart === 'price' ? d.price_error_pct : d.queries ? d.finds / d.queries * 100 : null,
    queries: d.queries, finds: d.finds, x: days.length > 1 ? 35 + i * 930 / (days.length - 1) : 500
  })));
  const step = $derived(days.length > 1 ? 930 / (days.length - 1) : 930);
  const max = $derived((Math.max(0, ...points.map(p => p.value ?? 0)) || 1) * 1.15);
  const valid = $derived(points.filter(p => p.value != null));
  // O ponto sob o mouse; sem mouse, o dia mais recente com dado.
  const shown = $derived(valid.find(p => p.day === hover) ?? valid[valid.length - 1]);
  const pct = (n: number) => `${n.toLocaleString('pt-BR', { maximumFractionDigits: 1 })}%`;
  const number = (n: number) => n.toLocaleString('pt-BR');
  const per100 = (n: number) => n.toLocaleString('pt-BR', { maximumFractionDigits: 2 });
  const date = (d: string) => d.slice(5).split('-').reverse().join('/');
  const when = (ts: number) => new Date(ts * 1000).toLocaleDateString('pt-BR', { timeZone: 'America/Sao_Paulo' });
  const y = (value: number) => 174 - value / max * 150;
</script>

<div class="dashboard">
  <div class="bar">
    <dl class="stats">
      <div><dt>Previsões avaliadas</dt><dd>{number(samples)}</dd><small>preço observado em 48 h</small></div>
      <div><dt>Achados por 100 consultas</dt><dd>{rate === null ? '—' : per100(rate)}</dd><small>{number(finds)} em {number(queries)}</small></div>
      <div><dt>Resultado virtual</dt><dd>{data.teacher.final_alerts ? signed(data.teacher.profit_cents) : '—'}</dd><small>{data.teacher.final_alerts ? `${number(data.teacher.final_alerts)} avisos avaliados` : 'aguardando avaliações'}</small></div>
      <div><dt>Em acompanhamento</dt><dd>{number(data.teacher.waiting)}</dd><small>compras virtuais</small></div>
    </dl>
    <div class="seg" role="group" aria-label="Período">
      {#each [7, 30, 60] as n}
        <button class:active={period === n} aria-pressed={period === n} onclick={() => period = n}>{n} dias</button>
      {/each}
    </div>
  </div>

  <div class="chart-head">
    <div>
      <h3>{chart === 'price' ? 'Precisão das previsões' : 'Achados por consulta'}</h3>
      <p>{chart === 'price' ? 'Erro típico entre o preço previsto e o observado em 48 h. Menor é melhor.' : 'Achados a cada 100 consultas de anúncios.'}</p>
    </div>
    <div class="seg" role="group" aria-label="Gráfico">
      <button class:active={chart === 'price'} aria-pressed={chart === 'price'} onclick={() => chart = 'price'}>Previsões</button>
      <button class:active={chart === 'finds'} aria-pressed={chart === 'finds'} onclick={() => chart = 'finds'}>Busca</button>
    </div>
  </div>

  {#if shown}
    <div class="plot">
      <div class="plot-area"><div class="y-axis"><span>{chart === 'price' ? pct(max) : per100(max)}</span><span>0</span></div>
      <svg viewBox="0 0 1000 210" role="img" aria-label={chart === 'price' ? 'Erro típico diário das previsões. Valores exatos em “Números por dia”.' : 'Achados diários a cada 100 consultas. Valores exatos em “Números por dia”.'} preserveAspectRatio="none" onpointerleave={() => hover = ''}>
        <line x1="35" y1="24" x2="965" y2="24" class="gridline" />
        <line x1="35" y1="99" x2="965" y2="99" class="gridline" />
        <line x1="35" y1="174" x2="965" y2="174" class="gridline" />
        <line x1={shown.x} y1="24" x2={shown.x} y2="174" class="cursor" />
        {#each points as p, i}
          {#if p.value != null}
            {#if i > 0 && points[i - 1].value != null}
              <line x1={points[i - 1].x} y1={y(points[i - 1].value!)} x2={p.x} y2={y(p.value)} class="trace" />
            {/if}
            <circle cx={p.x} cy={y(p.value)} r={p === shown ? 6 : 4} class="dot" />
            <!-- svelte-ignore a11y_no_static_element_interactions (os mesmos números estão em "Números por dia") -->
            <rect x={p.x - step / 2} y="0" width={step} height="210" class="hit" onpointerenter={() => hover = p.day} />
          {/if}
        {/each}
      </svg></div>
      <div class="axis"><span>{date(days[0].day)}</span><span>{date(days[days.length - 1].day)}</span></div>
    </div>
    <p class="readout" aria-live="off"><b>{date(shown.day)}</b>
      {chart === 'price' ? `${pct(shown.value!)} de erro típico em ${number(shown.samples)} previsões` : `${per100(shown.value!)} por 100 consultas · ${number(shown.finds)} achados em ${number(shown.queries)}`}</p>
  {:else}
    <div class="empty-chart"><strong>Ainda sem dados</strong><span>{chart === 'price' ? 'Aparece depois de 48 h acompanhando as previsões.' : 'Aparece quando houver consultas de anúncios.'}</span></div>
  {/if}

  <details class="numbers">
    <summary>Números por dia</summary>
    <div class="table-wrap">
      <table>
        <thead><tr><th>Dia</th><th>Consultas</th><th>Achados</th><th>Por 100 consultas</th><th>Previsões avaliadas</th><th>Erro típico</th><th title="Mediana de (observado − previsto) ÷ previsto. Negativo: o mercado pagou abaixo do previsto.">Observado vs. previsto</th></tr></thead>
        <tbody>{#each [...days].reverse() as d}
          <tr><td>{date(d.day)}</td><td>{number(d.queries)}</td><td>{number(d.finds)}</td><td>{d.queries ? per100(d.finds / d.queries * 100) : '—'}</td><td>{number(d.price_samples ?? 0)}</td><td>{d.price_error_pct == null ? '—' : pct(d.price_error_pct)}</td><td>{d.price_bias_pct == null ? '—' : `${d.price_bias_pct > 0 ? '+' : ''}${pct(d.price_bias_pct)}`}</td></tr>
        {/each}</tbody>
      </table>
    </div>
    <p>Observado vs. previsto negativo: o mercado pagou abaixo do previsto.</p>
  </details>
  <details class="numbers">
    <summary>Ajustes nas previsões</summary>
    {#if data.adjustments?.length}
      <ul class="adjustments">{#each data.adjustments as adjustment}
        <li><b>{when(adjustment.at)}</b><span>Fator: {adjustment.old == null ? '—' : pct(adjustment.old * 100)} → {adjustment.new == null ? '—' : pct(adjustment.new * 100)}</span><small>{adjustment.why}</small></li>
      {/each}</ul>
    {:else}<p>Nenhum ajuste registrado.</p>{/if}
  </details>
  <p class="disclaimer">Resultado virtual não é venda real. Histórico de até 60 dias.</p>
</div>

<style>
  .bar, .chart-head { display: flex; justify-content: space-between; align-items: flex-start; gap: 16px 32px; }
  .bar { padding-bottom: 20px; border-bottom: 1px solid var(--line); }
  .bar .stats { flex: 1; }
  .chart-head { align-items: center; margin-top: 20px; }
  h3 { font-size: 15px; }
  .chart-head p, .numbers p { color: var(--muted); font-size: 13px; margin-top: 3px; max-width: 75ch; }
  .seg { display: flex; gap: 3px; padding: 3px; background: var(--panel-2); border-radius: 7px; flex-shrink: 0; }
  .seg button { border: 0; background: transparent; color: var(--muted); padding: 0 12px; min-height: 30px; border-radius: 5px; white-space: nowrap; font-size: 13px; }
  .seg button:hover { color: var(--ink); }
  .seg button.active { background: #303a43; color: var(--ink); font-weight: 650; }
  .plot { margin-top: 16px; }
  svg { display: block; width: 100%; height: 190px; overflow: visible; }
  .plot-area { display: flex; gap: 8px; align-items: stretch; }
  .y-axis { width: 48px; flex-shrink: 0; display: flex; flex-direction: column; justify-content: space-between; padding: 18px 0 30px; color: var(--muted); font-size: 11px; text-align: right; }
  .gridline { stroke: var(--line); stroke-width: 1; vector-effect: non-scaling-stroke; }
  .cursor { stroke: var(--faint); stroke-width: 1; stroke-dasharray: 3 3; vector-effect: non-scaling-stroke; }
  .trace { stroke: var(--green); stroke-width: 2; vector-effect: non-scaling-stroke; }
  .dot { fill: var(--green); transition: r 0.15s; }
  .hit { fill: transparent; }
  .axis { display: flex; justify-content: space-between; margin-left: 60px; color: var(--muted); font-size: 12px; }
  .readout { margin: 10px 0 0 60px; color: var(--muted); font-size: 13px; }
  .readout b { color: var(--ink); margin-right: 6px; }
  .disclaimer { font-size: 12px; color: var(--muted); margin-top: 12px; }
  .empty-chart { min-height: 155px; display: flex; flex-direction: column; align-items: center; justify-content: center; text-align: center; gap: 6px; color: var(--muted); }
  .empty-chart strong { color: var(--ink); }
  .numbers { margin-top: 16px; border-top: 1px solid var(--line); }
  .numbers summary { cursor: pointer; padding: 14px 0 8px; font-weight: 600; }
  .table-wrap { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; white-space: nowrap; text-align: right; font-size: 13px; }
  th, td { padding: 8px 12px; border-bottom: 1px solid var(--line); }
  th { color: var(--muted); font-weight: 500; font-size: 12px; }
  th:first-child, td:first-child { text-align: left; padding-left: 0; }
  .adjustments { list-style: none; margin: 0; padding: 0; }
  .adjustments li { display: flex; flex-wrap: wrap; gap: 4px 14px; padding: 9px 0; border-bottom: 1px solid var(--line); }
  .adjustments small { flex-basis: 100%; color: var(--muted); }
  @media (max-width: 899px) {
    .bar, .chart-head { flex-direction: column; align-items: stretch; }
    .seg { align-self: flex-start; }
    svg { height: 150px; }
    .readout { margin-left: 0; }
  }
</style>
