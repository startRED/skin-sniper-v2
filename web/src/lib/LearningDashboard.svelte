<script lang="ts">
  import type { Learning } from './types';
  import { signed } from './format';

  let { data }: { data: Learning } = $props();
  let period = $state(30);
  let chart = $state<'price' | 'finds'>('price');
  let selectedDay = $state('');
  const days = $derived(data.days.slice(0, period).reverse());
  const queries = $derived(days.reduce((n, d) => n + d.queries, 0));
  const finds = $derived(days.reduce((n, d) => n + (d.queries ? d.finds : 0), 0));
  const samples = $derived(days.reduce((n, d) => n + (d.price_samples ?? 0), 0));
  const rate = $derived(queries ? finds / queries * 100 : null);
  const points = $derived(days.map((d, i) => ({
    day: d.day, samples: d.price_samples ?? 0, bias: d.price_bias_pct,
    value: chart === 'price' ? d.price_error_pct : d.queries ? d.finds / d.queries * 100 : null,
    queries: d.queries, finds: d.finds, x: days.length > 1 ? 35 + i * 930 / (days.length - 1) : 500
  })));
  const max = $derived((Math.max(0, ...points.map(p => p.value ?? 0)) || 1) * 1.15);
  const hasPoints = $derived(points.some(p => p.value != null));
  const selected = $derived(points.find(p => p.day === selectedDay && p.value != null));
  const pct = (n: number) => `${n.toLocaleString('pt-BR', { maximumFractionDigits: 1 })}%`;
  const number = (n: number) => n.toLocaleString('pt-BR');
  const date = (d: string) => d.slice(5).split('-').reverse().join('/');
  const when = (ts: number) => new Date(ts * 1000).toLocaleDateString('pt-BR', { timeZone: 'America/Sao_Paulo' });
  const y = (value: number) => 174 - value / max * 150;
</script>

<section class="dashboard" aria-label="Evolução do aprendizado">
  <div class="intro">
    <div>
      <h3>Evolução do aprendizado</h3>
      <p>Previsões e oportunidades ao longo do tempo.</p>
    </div>
    <div class="period" aria-label="Período">
      {#each [7, 30, 60] as n}
        <button class:active={period === n} aria-pressed={period === n} onclick={() => period = n}>{n} dias</button>
      {/each}
    </div>
  </div>

  <div class="overview">
    <div><span>Previsões avaliadas</span><strong>{number(samples)}</strong><small>com preço observado em 48 h</small></div>
    <div><span>Eficiência da busca</span><strong>{rate === null ? '—' : `${rate.toLocaleString('pt-BR', { maximumFractionDigits: 2 })}`}</strong><small>achados a cada 100 consultas</small></div>
    <div><span>Resultado virtual</span><strong>{data.teacher.final_alerts ? signed(data.teacher.profit_cents) : '—'}</strong><small>{data.teacher.final_alerts ? `${number(data.teacher.final_alerts)} avisos comuns avaliados · histórico disponível` : 'aguardando avaliações finais'}</small></div>
    <div><span>Em acompanhamento</span><strong>{number(data.teacher.waiting)}</strong><small>compras virtuais aguardando resultado · histórico disponível</small></div>
  </div>

  <div class="chart-head">
    <div>
      <h4>{chart === 'price' ? 'Precisão das previsões' : 'Achados por consulta'}</h4>
      <p>{chart === 'price' ? 'Erro típico diário entre o preço previsto e o observado em 48 h. Quanto menor, mais próximos.' : 'Achados a cada 100 consultas de anúncios. O mercado também influencia esse resultado.'}</p>
    </div>
    <div class="switch" aria-label="Escolher gráfico">
      <button class:active={chart === 'price'} aria-pressed={chart === 'price'} onclick={() => chart = 'price'}>Previsões</button>
      <button class:active={chart === 'finds'} aria-pressed={chart === 'finds'} onclick={() => chart = 'finds'}>Busca</button>
    </div>
  </div>

  {#if hasPoints}
    <div class="plot">
      <div class="plot-area"><div class="y-axis"><span>{chart === 'price' ? pct(max) : max.toLocaleString('pt-BR', { maximumFractionDigits: 2 })}</span><span>0</span></div>
      <svg viewBox="0 0 1000 210" role="img" aria-label={chart === 'price' ? 'Erro típico diário das previsões. Valores exatos na tabela abaixo.' : 'Achados diários a cada 100 consultas. Valores exatos na tabela abaixo.'} preserveAspectRatio="none">
        <line x1="35" y1="24" x2="965" y2="24" class="gridline" />
        <line x1="35" y1="99" x2="965" y2="99" class="gridline" />
        <line x1="35" y1="174" x2="965" y2="174" class="gridline" />
        {#each points as p, i}
          {#if p.value != null}
            {#if i > 0 && points[i - 1].value != null}
              <line x1={points[i - 1].x} y1={y(points[i - 1].value!)} x2={p.x} y2={y(p.value)} class="trace" />
            {/if}
            <circle cx={p.x} cy={y(p.value)} r="4" class="dot"><title>{date(p.day)}: {chart === 'price' ? `${pct(p.value)} em ${p.samples} previsões` : `${p.value.toLocaleString('pt-BR', { maximumFractionDigits: 2 })} por 100 consultas (${p.finds} em ${p.queries})`}</title></circle>
          {/if}
        {/each}
      </svg></div>
      <div class="axis"><span>{date(days[0].day)}</span><span>{date(days[days.length - 1].day)}</span></div>
    </div>
    <p class="chart-note">{chart === 'price' ? 'Avisos de skins comuns com preço observado. Poucos casos podem oscilar bastante.' : 'Dias sem consultas ficam sem ponto. Hoje ainda está em andamento.'}</p>
    <div class="point-detail"><label for="learning-day">Detalhes de um dia</label><select id="learning-day" value={selected?.day ?? ''} onchange={(e) => selectedDay = e.currentTarget.value}>
      <option value="">Escolha uma data</option>
      {#each points.filter(p => p.value != null).reverse() as point}<option value={point.day}>{date(point.day)}</option>{/each}
    </select>
    {#if selected && selected.value != null}<span>{chart === 'price' ? `${pct(selected.value)} de erro típico em ${number(selected.samples)} previsões` : `${selected.value.toLocaleString('pt-BR', { maximumFractionDigits: 2 })} por 100 consultas · ${number(selected.finds)} achados em ${number(selected.queries)} consultas`}</span>{/if}</div>
  {:else}
    <div class="empty-chart"><strong>Ainda não há dados para este gráfico</strong><span>{chart === 'price' ? 'Os preços aparecem depois que o app consegue acompanhar as previsões por 48 horas.' : 'Os pontos aparecem quando o app fizer consultas de anúncios.'}</span></div>
  {/if}

  <details class="numbers">
    <summary>Ver os números por dia</summary>
    <div class="table-wrap">
      <table>
        <thead><tr><th>Dia</th><th>Consultas</th><th>Achados</th><th>Por 100 consultas</th><th>Previsões avaliadas</th><th>Erro típico</th><th>Preço observado vs. previsto</th></tr></thead>
        <tbody>{#each [...days].reverse() as d}
          <tr><td>{date(d.day)}</td><td>{number(d.queries)}</td><td>{number(d.finds)}</td><td>{d.queries ? (d.finds / d.queries * 100).toLocaleString('pt-BR', { maximumFractionDigits: 2 }) : '—'}</td><td>{number(d.price_samples ?? 0)}</td><td>{d.price_error_pct == null ? '—' : pct(d.price_error_pct)}</td><td>{d.price_bias_pct == null ? '—' : `${d.price_bias_pct > 0 ? '+' : ''}${pct(d.price_bias_pct)}`}</td></tr>
        {/each}</tbody>
      </table>
    </div>
    <p>O desvio é a mediana de (observado − previsto) ÷ previsto. Negativo significa que os preços observados ficaram, tipicamente, abaixo do previsto.</p>
  </details>
  <details class="numbers">
    <summary>Histórico de ajustes nas previsões</summary>
    {#if data.adjustments?.length}
      <ul class="adjustments">{#each data.adjustments as adjustment}
        <li><b>{when(adjustment.at)}</b><span>Fator: {adjustment.old == null ? '—' : pct(adjustment.old * 100)} → {adjustment.new == null ? '—' : pct(adjustment.new * 100)}</span><small>{adjustment.why}</small></li>
      {/each}</ul>
    {:else}<p>Nenhum ajuste registrado no histórico disponível.</p>{/if}
  </details>
  <p class="disclaimer">Resultados virtuais não confirmam vendas. Histórico disponível: até 60 dias.</p>
</section>

<style>
  .dashboard { margin: 16px; padding: 20px; border: 1px solid var(--line); border-radius: 10px; background: var(--bg); }
  .intro, .chart-head { display: flex; justify-content: space-between; align-items: flex-start; gap: 16px; }
  h3 { font-size: 18px; line-height: 1.25; } h4 { font-size: 15px; }
  .intro p, .chart-head p, .numbers p { color: var(--muted); margin-top: 5px; max-width: 75ch; }
  .period, .switch { display: flex; gap: 3px; padding: 3px; background: var(--panel-2); border-radius: 7px; flex-shrink: 0; }
  button { border: 0; background: transparent; color: var(--muted); padding: 7px 10px; border-radius: 5px; white-space: nowrap; font-size: 12px; }
  button:hover { color: var(--ink); } button.active { background: #303a43; color: var(--ink); font-weight: 650; }
  .overview { display: grid; grid-template-columns: repeat(4, 1fr); gap: 20px; margin: 24px 0; padding: 18px 0; border-block: 1px solid var(--line); }
  .overview div { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
  .overview span, .overview small { color: var(--muted); font-size: 12px; } .overview strong { font-size: 25px; line-height: 1.15; font-weight: 650; }
  .chart-head { align-items: center; }
  .plot { margin-top: 20px; } svg { display: block; width: 100%; height: 190px; overflow: visible; }
  .plot-area { display: flex; gap: 8px; align-items: stretch; } .y-axis { width: 48px; flex-shrink: 0; display: flex; flex-direction: column; justify-content: space-between; padding: 18px 0 30px; color: var(--muted); font-size: 11px; text-align: right; }
  .gridline { stroke: var(--line); stroke-width: 1; vector-effect: non-scaling-stroke; } .trace { stroke: var(--green); stroke-width: 2; vector-effect: non-scaling-stroke; } .dot { fill: var(--green); vector-effect: non-scaling-stroke; }
  .axis { display: flex; justify-content: space-between; margin-left: 60px; color: var(--muted); font-size: 12px; }
  .chart-note, .disclaimer { font-size: 12px; color: var(--muted); margin-top: 12px; }
  .point-detail { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; margin-top: 14px; color: var(--muted); font-size: 12px; } .point-detail select { min-height: 36px; padding: 0 8px; color: var(--ink); background: var(--panel-2); border: 1px solid var(--line); border-radius: 6px; }
  .empty-chart { min-height: 155px; display: flex; flex-direction: column; align-items: center; justify-content: center; text-align: center; gap: 6px; color: var(--muted); }
  .empty-chart strong { color: var(--ink); } .numbers { margin-top: 20px; border-top: 1px solid var(--line); } .numbers summary { cursor: pointer; padding: 14px 0 8px; font-weight: 600; }
  .table-wrap { overflow-x: auto; } table { width: 100%; border-collapse: collapse; white-space: nowrap; text-align: right; font-size: 12px; } th, td { padding: 9px 12px; border-bottom: 1px solid var(--line); } th { color: var(--muted); font-weight: 500; } th:first-child, td:first-child { text-align: left; }
  .disclaimer { margin-top: 20px; padding-top: 12px; border-top: 1px solid var(--line); }
  .adjustments { list-style: none; margin: 0; padding: 0; } .adjustments li { display: flex; flex-wrap: wrap; gap: 4px 14px; padding: 9px 0; border-bottom: 1px solid var(--line); } .adjustments small { flex-basis: 100%; color: var(--muted); }
  @media (max-width: 700px) { .dashboard { margin: 12px 10px; padding: 15px; } .intro, .chart-head { flex-direction: column; } .overview { grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 20px 12px; } .overview strong { font-size: 22px; } svg { height: 150px; } }
</style>
