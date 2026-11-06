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
  const hv = $derived(data?.harvest ?? null);
  const wk = $derived(data?.weeks ?? null);
  const research = $derived(data?.research ?? null);
  const trial = $derived(data?.trial ?? null);
  const exploration = $derived(data?.exploration ?? null);
  const experiment = $derived(data?.experiment ?? null);
  const cohorts = $derived.by(() => {
    const groups = new Map<string, {
      policy: string | null; kind: string; total: number; mature: number; waiting: number;
      unlabeled: number; labeled: number; closed: number; predicted_cents: number;
      virtual_cents: number; indirect: number;
    }>();
    for (const c of data?.cohorts ?? []) {
      const key = `${c.policy ?? ''}|${c.kind}`;
      const row = groups.get(key) ?? {
        policy: c.policy, kind: c.kind, total: 0, mature: 0, waiting: 0, unlabeled: 0,
        labeled: 0, closed: 0, predicted_cents: 0, virtual_cents: 0, indirect: 0
      };
      for (const field of ['total', 'mature', 'waiting', 'unlabeled', 'labeled', 'closed', 'predicted_cents', 'virtual_cents', 'indirect'] as const) row[field] += c[field];
      groups.set(key, row);
    }
    return [...groups.values()];
  });
  const POLICY = {
    shadow: 'em teste silencioso: o modo atual escolhe, o modo colheita só mede',
    ucb: 'hoje no modo atual (dia de comparação)',
    harvest: 'hoje no modo colheita: consulta onde mais achado se acumulou'
  } as const;
  const share = (a: number, b: number) => (b ? `${Math.round((a / b) * 100)}%` : '—');
  const life = (s: number) => (s < 120 ? `${Math.round(s)} s` : `${Math.round(s / 60)} min`);
  const of = (a: number, b: number) => (b ? `${a} de ${b}` : '—');
  const ratio = (r: number | null) => (r == null ? '—' : `${r.toFixed(2).replace('.', ',')}×`);
  const trialState = (phase: string) => ({
    baseline: 'Linha de base', control: 'Controle', pilot: 'Piloto', paused: 'Pausado', complete: 'Concluído'
  } as Record<string, string>)[phase.toLowerCase()] ?? phase;
  const policyName = (policy: string | null) => ({
    ucb: 'Atual', harvest: 'Colheita', shadow: 'Sombra', baseline: 'Linha de base', control: 'Controle', pilot: 'Piloto'
  } as Record<string, string>)[policy?.toLowerCase() ?? ''] ?? policy ?? 'Sem política';
  const kindName = (kind: string) => ({ comum: 'Comuns', common: 'Comuns', raro: 'Raros', rare: 'Raros' } as Record<string, string>)[kind.toLowerCase()] ?? kind;
  const conclusionName = (conclusion: string) => ({
    inconclusivo: 'inconclusivo', inseguro: 'interrompido por segurança', elegivel: 'apto para avaliação'
  } as Record<string, string>)[conclusion.toLowerCase()] ?? conclusion;
  const days = $derived((data?.days ?? []).filter((d, i) => i === 0 || d.queries + d.books + d.new_listings > 0));
</script>

<details class="fold">
  <summary>
    Aprendizado
    {#if today}<span class="muted"> · hoje: {today.new_listings} anúncios novos, {today.sales} saíram da lista, {today.finds} {today.finds === 1 ? 'achado' : 'achados'}</span>{/if}
  </summary>
  <div class="body">
    <p class="muted sub">
      O app guarda cada anúncio que vê e confere com o mercado o que aconteceu com ele, sem comprar nada. É daqui que ele
      aprende quanto os anúncios duram e procura indícios do que pode ter sido vendido; sumir da lista não confirma uma venda.{#if data?.since}{' '}Registro desde {day(data.since)}.{/if}
    </p>
    {#if error}<p class="amber">{error}</p>{/if}
    {#if trial || exploration || research || experiment}
      <section class="ml" aria-label="Exploração e pesquisa">
        {#if trial}
          <div class="ml-head"><b>{trialState(trial.phase)}</b>{#if trial.block_id != null}<span class="muted">bloco {trial.block_id}</span>{/if}<span class="muted">política de referência: {policyName(trial.base_policy)}</span></div>
          {#if trial.reason}<p class="muted ml-reason">{trial.reason}</p>{/if}
        {/if}
        {#if exploration}
          <p><b>Candidata:</b> {exploration.candidate ?? 'nenhuma no momento'} · {exploration.visits} visitas · {exploration.targets_tested} alvos testados · {exploration.unique_finds} achados únicos{#if exploration.retained} · mantida para acompanhamento{/if}</p>
        {/if}
        {#if research}
          <p><b>Última hora:</b> {research.reads_1h} tentativas de leitura na Steam · exploração {research.exploratory_1h}/{research.exploratory_limit} (até {research.exploratory_limit}; não é cota garantida) · {research.available} vagas de exploração disponíveis. CSFloat: {research.csfloat_1h} leituras.</p>
          <p class="muted">Acompanhamento: {research.followup_1h} leituras · respostas 429: {research.limited_1h} · erros: {research.failed_1h}.</p>
        {/if}
        {#if experiment}
          <p><b>Comparação:</b> {experiment.pairs} {experiment.pairs === 1 ? 'par' : 'pares'} de blocos · {conclusionName(experiment.conclusion)}{#if experiment.reason} · {experiment.reason}{/if}</p>
        {/if}
      </section>
    {/if}
    {#if cohorts.length}
      <section class="cohorts" aria-label="Resultados por grupo de comparação">
        <h3>Grupos de comparação</h3>
        <p class="muted">Previsto e virtual são estimativas. O resultado virtual NÃO é uma venda real; venda real informada ainda não aparece neste placar.</p>
        {#each cohorts as c (`${c.policy ?? ''}|${c.kind}`)}
          <div class="cohort">
            <b>{policyName(c.policy)} · {kindName(c.kind)}</b>
            <span>{c.total} registros · maduros {c.mature} · pendentes {c.waiting} · sem resultado {c.unlabeled}</span>
            <span>Rotulados {c.labeled} · fechados {c.closed}{#if c.indirect} ({c.indirect} por indício indireto){/if}</span>
            <span>Lucro previsto {money(c.predicted_cents)} · lucro virtual {c.labeled ? signed(c.virtual_cents) : '—'}</span>
          </div>
        {/each}
      </section>
    {/if}
    {#if t && t.ghosts}
      <div class="teacher" aria-label="Professor">
        <div><span class="muted">Compras virtuais</span><b>{t.ghosts}</b><small class="muted">{t.waiting} esperando o resultado</small></div>
        <div title="Nos casos acompanhados, o preço observado chegou ao previsto em até 48 h; isso não confirma uma venda."><span class="muted">Avisos com preço previsto em 48 h</span><b>{of(t.fast_alerts_closed, t.fast_alerts)}</b><small class="muted">quase-achados: {of(t.fast_near_closed, t.fast_near)}</small></div>
        <div title="Mediana dos preços observados nas 48 h dividida pelo previsto. Abaixo de 1 = o app prevê alto."><span class="muted">Mercado ÷ previsto</span><b class:amber={t.fast_ratio != null && t.fast_ratio < 0.95}>{ratio(t.fast_ratio)}</b><small class="muted">comuns, 48 h</small></div>
        <div title="Vendendo depois dos 7 dias de trava, pelo menor entre o histórico e o livro"><span class="muted">Lucro virtual (7 dias)</span><b class:up={t.profit_cents > 0}>{t.final_alerts ? signed(t.profit_cents) : '—'}</b><small class="muted">{t.final_alerts ? `${of(t.final_alerts_won, t.final_alerts)} com lucro · previsto ${money(t.predicted_cents)}` : 'primeiro resultado no 10º dia'}</small></div>
        {#if t.rare_labeled}<div><span class="muted">Raros com indício de fechamento</span><b>{of(t.rare_closed, t.rare_labeled)}</b><small class="muted">em 14 dias</small></div>{/if}
      </div>
    {/if}
    {#if hv || wk}
      <ul class="notes muted">
        {#if hv}
          <li>
            <b>Agenda</b> {POLICY[hv.policy]}.
            Um achado dura ~{life(hv.life_s)} {hv.lives ? `(medido em ${hv.lives} sumiços)` : '(padrão, ainda sem medida)'}.
            {#if hv.picks}As duas escolheriam o mesmo alvo em {share(hv.agree, hv.picks)} das vezes.{/if}
            {#if hv.ucb_rate != null && hv.harvest_rate != null}Comparação: atual {(hv.ucb_rate * 100).toFixed(1).replace('.', ',')} · colheita {(hv.harvest_rate * 100).toFixed(1).replace('.', ',')} achados e quase por 100 consultas.{/if}
          </li>
        {/if}
        {#if data && data.refit < 1}
          <li><b>Preço do comum</b> descontado {Math.round((1 - data.refit) * 100)}% pelo professor (o mercado pagou abaixo do previsto).</li>
        {/if}
        {#if wk}
          <li>
            <b>Semana</b> {wk[0].finds} {wk[0].finds === 1 ? 'achado' : 'achados'}, {wk[0].near} quase, {wk[0].queries} consultas{#if wk[0].alerts}; avisos que venderiam em 48 h: {wk[0].alerts_closed} de {wk[0].alerts}{/if}{#if wk[0].labeled}; lucro virtual {signed(wk[0].profit_cents)} em {wk[0].labeled}{/if}.
            {#if wk[1].queries}Semana anterior: {wk[1].finds} achados, {wk[1].near} quase, {wk[1].queries} consultas{#if wk[1].labeled}; lucro virtual {signed(wk[1].profit_cents)}{/if}.{/if}
          </li>
        {/if}
      </ul>
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
  .ml, .cohorts { margin: 0 16px 12px; padding: 10px 12px; border: 1px solid var(--line); border-radius: 8px; display: grid; gap: 5px; }
  .ml p, .cohorts p { margin: 0; }
  .ml-head { display: flex; flex-wrap: wrap; gap: 4px 12px; }
  .ml-reason { font-size: 13px; }
  .cohorts h3 { margin: 0; font-size: 14px; }
  .cohorts > p { font-size: 13px; }
  .cohort { display: grid; gap: 2px; padding: 7px 0; border-top: 1px solid var(--line); font-size: 13px; }
  .cohort > span { color: var(--muted); }
  .notes { margin: 0 16px 12px; padding-left: 18px; max-width: 110ch; display: grid; gap: 4px; }
  .notes b { color: var(--ink); font-weight: 600; }
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
    .ml, .cohorts { margin-left: 10px; margin-right: 10px; }
    .head { display: none; }
    .grid { grid-template-columns: repeat(3, 1fr); row-gap: 8px; padding: 10px 14px; }
    .c::before { display: block; color: var(--faint); font-size: 11px; }
  }
</style>
