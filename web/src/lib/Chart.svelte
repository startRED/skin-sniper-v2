<script lang="ts">
  import { day, money } from './format';
  import type { History } from './types';

  let { history, paid, now }: { history: History; paid: number; now: number } = $props();

  let w = $state(640);
  const H = 230;
  const narrow = $derived(w < 520);
  const M = $derived({ l: narrow ? 50 : 56, r: narrow ? 62 : 132, t: 12, b: 26 });

  const pts = $derived(history.points);
  const t1 = $derived(now);
  const t0 = $derived(now - 30 * 86400);
  const lines = $derived([
    { key: 'high', label: 'Alto', cents: history.sell.high.price_cents, cls: 'hi' },
    { key: 'mid', label: 'Médio', cents: history.sell.mid.price_cents, cls: 'mid' },
    { key: 'quick', label: 'Rápido', cents: history.sell.quick.price_cents, cls: 'qk' },
    { key: 'paid', label: 'Você paga', cents: paid, cls: 'paid' }
  ]);
  const ys = $derived([...pts.map((p) => p[1]), ...lines.map((l) => l.cents)]);
  const lo = $derived(Math.min(...ys));
  const hi = $derived(Math.max(...ys));
  const pad = $derived((hi - lo || 100) * 0.12);
  const yMin = $derived(lo - pad);
  const yMax = $derived(hi + pad);
  const X = (t: number) => M.l + ((t - t0) / (t1 - t0)) * (w - M.l - M.r);
  const Y = (c: number) => M.t + (1 - (c - yMin) / (yMax - yMin)) * (H - M.t - M.b);
  const path = $derived(pts.map((p, i) => `${i ? 'L' : 'M'}${X(p[0]).toFixed(1)} ${Y(p[1]).toFixed(1)}`).join(''));
  const maxBuy = $derived(Math.max(1, ...pts.map((p) => p[2])));
  const bars = $derived(
    pts.map((p) => `M${X(p[0]).toFixed(1)} ${H - M.b}v${(-(p[2] / maxBuy) * 26).toFixed(1)}`).join('')
  );
  const gridY = $derived([0, 1, 2, 3].map((i) => yMin + ((yMax - yMin) * i) / 3));
  const ticksX = $derived([0, 10, 20, 30].map((d) => t0 + d * 86400));

  // rótulos à direita não podem se sobrepor: empurra para baixo quando colidem
  const labels = $derived.by(() => {
    const sorted = lines.map((l) => ({ ...l, y: Y(l.cents) })).sort((a, b) => a.y - b.y);
    for (let i = 1; i < sorted.length; i++) sorted[i].y = Math.max(sorted[i].y, sorted[i - 1].y + 14);
    return sorted;
  });

  let hover = $state<number | null>(null);
  function move(e: PointerEvent) {
    const r = (e.currentTarget as SVGElement).getBoundingClientRect();
    const t = t0 + ((e.clientX - r.left - M.l) / (w - M.l - M.r)) * (t1 - t0);
    let best = 0;
    pts.forEach((p, i) => { if (Math.abs(p[0] - t) < Math.abs(pts[best][0] - t)) best = i; });
    hover = best;
  }
  const hp = $derived(hover != null ? pts[hover] : null);
  const summary = $derived(
    `Preço mediano dos últimos 30 dias, de ${money(Math.min(...pts.map((p) => p[1])))} a ${money(Math.max(...pts.map((p) => p[1])))}. ` +
      `Preços de venda: alto ${money(history.sell.high.price_cents)}, médio ${money(history.sell.mid.price_cents)}, rápido ${money(history.sell.quick.price_cents)}.`
  );
</script>

<div class="wrap" bind:clientWidth={w}>
  {#if pts.length < 2}
    <p class="muted empty">Ainda não há histórico suficiente desta skin para desenhar o gráfico.</p>
  {:else}
    <svg width={w} height={H} role="img" aria-label={summary} onpointermove={move} onpointerleave={() => (hover = null)}>
      {#each gridY as g}
        <line x1={M.l} x2={w - M.r} y1={Y(g)} y2={Y(g)} class="grid" />
        <text x={M.l - 8} y={Y(g) + 4} class="ax" text-anchor="end">{money(Math.round(g))}</text>
      {/each}
      {#each ticksX as t}
        <text x={X(t)} y={H - 6} class="ax" text-anchor={t === t0 ? 'start' : t === ticksX[3] ? 'end' : 'middle'}>{day(t)}</text>
      {/each}
      <path d={bars} class="bars" />
      {#each lines as l}
        <line x1={M.l} x2={w - M.r} y1={Y(l.cents)} y2={Y(l.cents)} class="ln {l.cls}" />
      {/each}
      <path d={path} class="med" />
      {#each labels as l}
        <text x={w - M.r + 8} y={l.y + 4} class="lb {l.cls}">{narrow ? l.label : `${l.label} ${money(l.cents)}`}</text>
      {/each}
      {#if hp}
        <line x1={X(hp[0])} x2={X(hp[0])} y1={M.t} y2={H - M.b} class="cross" />
        <circle cx={X(hp[0])} cy={Y(hp[1])} r="3.5" class="dotc" />
      {/if}
    </svg>
    <div class="tip" aria-hidden="true">
      {#if hp}{day(hp[0])} · mediana <b>{money(hp[1])}</b> · {hp[2]} {hp[2] === 1 ? 'compra' : 'compras'}{:else}Mediana do preço pago nos últimos 30 dias; as barras de baixo são o número de compras.{/if}
    </div>
  {/if}
</div>

<style>
  .wrap { width: 100%; min-width: 0; overflow: hidden; }
  svg { display: block; touch-action: pan-y; }
  .grid { stroke: var(--line); stroke-width: 1; }
  .ax { fill: var(--faint); font-size: 11px; }
  .bars { stroke: #2d3742; stroke-width: 1.5; }
  .med { fill: none; stroke: #c3ccd5; stroke-width: 1.6; stroke-linejoin: round; }
  .ln { stroke-width: 1.5; stroke-dasharray: 6 4; }
  .ln.hi { stroke: #8be6bd; }
  .ln.mid { stroke: var(--green); stroke-dasharray: none; stroke-width: 2; }
  .ln.qk { stroke: var(--amber); }
  .ln.paid { stroke: var(--ink); stroke-dasharray: 2 4; opacity: 0.75; }
  .lb { font-size: 11px; font-weight: 600; }
  .lb.hi { fill: #8be6bd; }
  .lb.mid { fill: var(--green); }
  .lb.qk { fill: var(--amber); }
  .lb.paid { fill: var(--muted); }
  .cross { stroke: var(--faint); stroke-width: 1; }
  .dotc { fill: var(--ink); }
  .tip { min-height: 20px; margin-top: 4px; color: var(--muted); font-size: 12px; }
  .tip b { color: var(--ink); }
  .empty { padding: 40px 0; text-align: center; }
</style>
