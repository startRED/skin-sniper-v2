<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from './api';
  import { day, money, signed } from './format';
  import type { Learning } from './types';

  let data = $state<Learning | null>(null);
  let error = $state('');

  async function load() {
    try {
      data = await api<Learning>('/api/learning');
      error = '';
    } catch (e) {
      error = e instanceof Error ? e.message : 'Não consegui ler o placar.';
    }
  }
  onMount(() => {
    load();
    const t = setInterval(load, 5 * 60 * 1000);
    return () => clearInterval(t);
  });

  const today = $derived(data?.days[0] ?? null);
  const min = (m: number | null) => (m == null ? '—' : m < 1 ? '< 1 min' : m < 120 ? `${Math.round(m)} min` : `${(m / 60).toFixed(1).replace('.', ',')} h`);
  const label = (d: string) => d.split('-').reverse().slice(0, 2).join('/');
  const t = $derived(data?.teacher ?? null);
  const of = (a: number, b: number) => (b ? `${a} de ${b}` : '—');
  const ratio = (r: number | null) => (r == null ? '—' : `${r.toFixed(2).replace('.', ',')}×`);
  const days = $derived((data?.days ?? []).filter((d, i) => i === 0 || d.queries + d.books + d.new_listings > 0));
</script>

<details class="fold">
  <summary>
    Aprendizado
    {#if today}<span class="muted"> · hoje: {today.new_listings} anúncios novos, {today.sales} sumiram (vendidos), {today.finds} {today.finds === 1 ? 'achado' : 'achados'}</span>{/if}
  </summary>
  <div class="body">
    <p class="muted sub">
      O app guarda cada anúncio que vê e confere com o mercado o que aconteceu com ele, sem comprar nada. É daqui que ele
      aprende quanto um achado dura e o que fecha de verdade.{#if data?.since}{' '}Registro desde {day(data.since)}.{/if}
    </p>
    {#if error}<p class="amber">{error}</p>{/if}
    {#if t && t.ghosts}
      <div class="teacher" aria-label="Professor">
        <div><span class="muted">Compras virtuais</span><b>{t.ghosts}</b><small class="muted">{t.waiting} esperando o resultado</small></div>
        <div title="Nas 48 h depois do aviso, vendeu a pelo menos o preço que o app previu?"><span class="muted">Avisos que venderiam em 48 h</span><b>{of(t.fast_alerts_closed, t.fast_alerts)}</b><small class="muted">quase-achados: {of(t.fast_near_closed, t.fast_near)}</small></div>
        <div title="Mediana do preço real das 48 h dividido pelo previsto. Abaixo de 1 = o app prevê alto."><span class="muted">Real ÷ previsto</span><b class:amber={t.fast_ratio != null && t.fast_ratio < 0.95}>{ratio(t.fast_ratio)}</b><small class="muted">comuns, 48 h</small></div>
        <div title="Vendendo depois dos 7 dias de trava, pelo menor entre o histórico e o livro"><span class="muted">Lucro virtual (7 dias)</span><b class:up={t.profit_cents > 0}>{t.final_alerts ? signed(t.profit_cents) : '—'}</b><small class="muted">{t.final_alerts ? `${of(t.final_alerts_won, t.final_alerts)} com lucro · previsto ${money(t.predicted_cents)}` : 'primeiro resultado no 10º dia'}</small></div>
        {#if t.rare_labeled}<div><span class="muted">Raros que fecharam</span><b>{of(t.rare_closed, t.rare_labeled)}</b><small class="muted">em 14 dias</small></div>{/if}
      </div>
    {/if}
    {#if days.length}
      <div class="table" role="table" aria-label="Placar do aprendizado por dia">
        <div class="head grid" role="row">
          <div role="columnheader">Dia</div>
          <div role="columnheader" title="Leituras de anúncios · leituras do livro de ofertas">Consultas · livro</div>
          <div role="columnheader" title="Anúncios vistos pela primeira vez">Anúncios novos</div>
          <div role="columnheader" title="Idade mediana do anúncio quando o app o viu (só os de idade conhecida)">Idade ao ver</div>
          <div role="columnheader" title="Sumiram sem o mesmo item voltar à venda · relistados pelo vendedor">Sumiram · relistados</div>
          <div role="columnheader" title="Quanto o anúncio que sumiu ficou à venda, na mediana">Duram</div>
          <div role="columnheader" title="Quase deram lucro · deram lucro">Quase · achados</div>
        </div>
        {#each days as d (d.day)}
          <div class="row grid" role="row">
            <div class="c" role="cell" data-l="Dia"><b>{label(d.day)}</b></div>
            <div class="c" role="cell" data-l="Consultas · livro">{d.queries} · {d.books}</div>
            <div class="c" role="cell" data-l="Anúncios novos">{d.new_listings}</div>
            <div class="c" role="cell" data-l="Idade ao ver">{min(d.age_median_min)} <small class="muted">({d.aged})</small></div>
            <div class="c" role="cell" data-l="Sumiram · relistados">{d.sales} · {d.relists}</div>
            <div class="c" role="cell" data-l="Duram">{min(d.life_median_min)}</div>
            <div class="c" role="cell" data-l="Quase · achados">{d.near} · <b class:up={d.finds > 0}>{d.finds}</b></div>
          </div>
        {/each}
      </div>
    {/if}
  </div>
</details>

<style>
  .fold { background: var(--panel); border-radius: var(--radius); }
  summary { cursor: pointer; padding: 12px 16px; font-weight: 600; }
  summary .muted { font-weight: 400; }
  .body { padding: 0 0 8px; }
  .sub { margin: 0 16px 10px; max-width: 110ch; }
  .amber { color: var(--amber); margin: 0 16px 8px; }
  .teacher { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 8px 16px; margin: 0 16px 12px; }
  .teacher div { display: flex; flex-direction: column; gap: 2px; }
  .teacher span, .teacher small { font-size: 12px; }
  .teacher b { font-size: 18px; }
  .teacher b.amber { margin: 0; }
  .grid {
    display: grid; align-items: center; column-gap: 12px; padding: 0 16px;
    grid-template-columns: minmax(60px, 0.6fr) repeat(6, minmax(90px, 1fr));
  }
  .head { min-height: 34px; border-bottom: 1px solid var(--line); color: var(--muted); font-size: 12px; }
  .row { min-height: 40px; border-bottom: 1px solid var(--line); }
  .c::before { content: attr(data-l); display: none; }
  @media (max-width: 899px) {
    .head { display: none; }
    .grid { grid-template-columns: repeat(3, 1fr); row-gap: 8px; padding: 10px 14px; }
    .c::before { display: block; color: var(--faint); font-size: 11px; }
  }
</style>
