<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from './api';
  import { day, money, signed } from './format';
  import type { Learning } from './types';
  import LearningDashboard from './LearningDashboard.svelte';

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
  const hv = $derived(data?.value ?? null);
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
  const CLASS = { d: 'baratos', r: 'seed raro' } as Record<string, string>;
  const perRead = (s: { worth_cents: number; reads: number }) => s.worth_cents / s.reads;
  const dec = (x: number, n = 2) => x.toFixed(n).replace('.', ',');
  const life = (s: number) => (s < 120 ? `${Math.round(s)} s` : `${Math.round(s / 60)} min`);
  const of = (a: number, b: number) => (b ? `${a} de ${b}` : '—');
  const ratio = (r: number | null) => (r == null ? '—' : `${r.toFixed(2).replace('.', ',')}×`);
  const trialState = (phase: string) => ({
    baseline: 'Linha de base', control: 'Controle', pilot: 'Piloto', paused: 'Pausado', complete: 'Concluído'
  } as Record<string, string>)[phase.toLowerCase()] ?? phase;
  const policyName = (policy: string | null) => ({
    valor: 'Valor por leitura', ucb: 'Atual', harvest: 'Colheita', shadow: 'Sombra', baseline: 'Linha de base', control: 'Controle', pilot: 'Piloto'
  } as Record<string, string>)[policy?.toLowerCase() ?? ''] ?? policy ?? 'Sem política';
  const kindName = (kind: string) => ({ comum: 'Comuns', common: 'Comuns', raro: 'Raros', rare: 'Raros' } as Record<string, string>)[kind.toLowerCase()] ?? kind;
  const conclusionName = (conclusion: string) => ({
    inconclusivo: 'inconclusivo', inseguro: 'interrompido por segurança', elegivel: 'apto para avaliação'
  } as Record<string, string>)[conclusion.toLowerCase()] ?? conclusion;
  const days = $derived((data?.days ?? []).filter((d, i) => i === 0 || d.queries + d.books + d.new_listings > 0));
</script>

{#snippet kv(label: string, value: string | number, hint?: string)}
  <div title={hint}><dt>{label}</dt><dd>{value}</dd></div>
{/snippet}

<section aria-labelledby="h-learn">
  <h2 id="h-learn" class="sr-only">Aprendizado</h2>
  {#if error}<p class="amber" role="alert">{error}</p>{/if}
  {#if !data && !error}<p class="muted">Carregando o placar…</p>{/if}
  {#if data}
    {#if today}
      <p class="today">Hoje: <b>{today.new_listings}</b> anúncios novos · <b>{today.sales}</b> saíram da lista · <b class:up={today.finds > 0}>{today.finds}</b> {today.finds === 1 ? 'achado' : 'achados'}{#if data.since}<span class="muted"> · registro desde {day(data.since)}</span>{/if}</p>
    {/if}
    <LearningDashboard {data} />

    <details class="more">
      <summary>Diagnóstico da busca</summary>
      <div class="groups">
        {#if data.candidates}
          {@const c = data.candidates}
          <div class="grp">
            <h3 title="Anúncios distintos avaliados nas últimas {c.hours} h. Acima do saldo bom e virtual não são compra possível nem entram no placar.">Candidatos por hora ({c.hours} h)</h3>
            <dl class="stats">
              <div><dt>Aprovadas</dt><dd>{dec(c.approved / c.hours)}</dd><small>{c.approved} anúncios</small></div>
              <div title="Passariam no lucro e no selo se coubessem no saldo; avisadas no Telegram com o que falta."><dt>Acima do saldo, boas</dt><dd>{dec(c.over_saldo / c.hours)}</dd><small>{c.over_saldo} anúncios</small></div>
              <div title="Raros sem lucro com prejuízo estimado até R$ 1 e perda pequena vendidos como comum. Sem compra e sem aviso; sumir não prova venda."><dt>Virtuais (zona amarela)</dt><dd>{dec(c.yellow / c.hours)}</dd><small>{c.yellow} anúncios · {c.yellow_gone} sumiram ({c.yellow_fast} em até 48 h sem voltar) · {c.yellow_relisted} relistados</small></div>
            </dl>
          </div>
        {/if}
        {#if t && t.ghosts}
          <div class="grp">
            <h3>Compras virtuais</h3>
            <dl class="stats">
              <div><dt>Acompanhadas</dt><dd>{t.ghosts}</dd><small>{t.waiting} esperando resultado</small></div>
              <div title="Nos casos acompanhados, o preço observado chegou ao previsto em até 48 h; isso não confirma uma venda."><dt>Avisos que bateram o previsto em 48 h</dt><dd>{of(t.fast_alerts_closed, t.fast_alerts)}</dd><small>quase-achados: {of(t.fast_near_closed, t.fast_near)}</small></div>
              <div title="Mediana dos preços observados nas 48 h dividida pelo previsto. Abaixo de 1 = o app prevê alto."><dt>Mercado ÷ previsto</dt><dd class:amber={t.fast_ratio != null && t.fast_ratio < 0.95}>{ratio(t.fast_ratio)}</dd><small>comuns, 48 h</small></div>
              <div title="Vendendo depois dos 7 dias de trava, pelo menor entre o histórico e o livro"><dt>Lucro virtual (7 dias)</dt><dd class:up={t.profit_cents > 0}>{t.final_alerts ? signed(t.profit_cents) : '—'}</dd><small>{t.final_alerts ? `${of(t.final_alerts_won, t.final_alerts)} com lucro · previsto ${money(t.predicted_cents)}` : 'primeiro resultado no 10º dia'}</small></div>
              {#if t.rare_labeled}<div><dt>Raros com indício de fechamento</dt><dd>{of(t.rare_closed, t.rare_labeled)}</dd><small>em 14 dias</small></div>{/if}
            </dl>
          </div>
        {/if}

        {#if hv}
          <div class="grp">
            <h3 title="A agenda lê a faixa de seed onde mais anúncio novo que cabe no saldo deve ter aparecido desde a última leitura, pesado pelo lucro que as vendas daquele seed provam.">Agenda de leituras</h3>
            <dl class="stats">
              {@render kv('Um achado dura', `~${life(hv.life_s)}`, hv.lives ? `Medido em ${hv.lives} sumiços` : 'Valor padrão, ainda sem medida')}
              {#if hv.reads}
                <div title="Últimos 7 dias: {hv.fresh} anúncios novos em {hv.reads} leituras"><dt>Novos por leitura (7 dias)</dt><dd>{dec(hv.fresh / hv.reads)}</dd><small>{hv.classes.filter((c) => c.reads).map((c) => `${CLASS[c.class] ?? c.class} ${dec(c.fresh / c.reads)}`).join(' · ')}</small></div>
              {/if}
              {#if hv.loss != null && hv.base_loss != null}
                <div title="Menor é melhor. A comparação é uma taxa fixa por tipo de busca."><dt>Erro de previsão</dt><dd>{dec(hv.loss, 3)}</dd><small>taxa fixa: {dec(hv.base_loss, 3)} · previu {Math.round(hv.predicted)}, vieram {Math.round(hv.actual)}</small></div>
              {/if}
              {#if hv.catalog_seeds}
                <div title="Seeds que as vendas do CSFloat provaram vender acima do preço comum. Só eles são lidos."><dt>Catálogo de pattern</dt><dd>{hv.catalog_seeds} seeds</dd><small>em {hv.catalog_skins} skins</small></div>
              {/if}
              {#if hv.policy?.reads && hv.control?.reads}
                <div title="1 leitura em 8 vai para a faixa lida há mais tempo, sem olhar valor nenhum (rodízio cego). Se a escolha da agenda não render mais por leitura que esse rodízio, o aprendizado não está ajudando. Valor = anúncios novos × lucro que a prova do seed promete."><dt>Agenda × rodízio cego (7 dias)</dt><dd class:amber={perRead(hv.policy) < perRead(hv.control)}>{perRead(hv.control) > 0 ? `${dec(perRead(hv.policy) / perRead(hv.control), 1)}×` : '—'}</dd><small>{money(Math.round(perRead(hv.policy)))} × {money(Math.round(perRead(hv.control)))} por leitura · {hv.policy.reads} × {hv.control.reads} leituras · achados {money(hv.policy.gain_cents)} × {money(hv.control.gain_cents)}</small></div>
              {/if}
              {#if hv.backtest_n && hv.backtest_paid != null && hv.backtest_ratio != null}
                <div title="Cada venda de seed do catálogo foi prevista só com as vendas anteriores a ela. Mostra quantas saíram por pelo menos 1,15× o preço comum (paga a taxa da Steam) e quanto a venda real ficou do previsto."><dt>Seed do catálogo que pagou</dt><dd>{Math.round(hv.backtest_paid * 100)}%</dd><small>{hv.backtest_n} vendas · real ÷ previsto {dec(hv.backtest_ratio, 2)}</small></div>
              {/if}
              <div title="Ajuste de hora em hora"><dt>Janela · memória</dt><dd>{dec(hv.window_s / 3600, 1)} h · {Math.round(hv.half_life_h)} h</dd></div>
              {#if data.refit < 1}{@render kv('Desconto no preço do comum', `${Math.round((1 - data.refit) * 100)}%`, 'O mercado pagou abaixo do previsto')}{/if}
            </dl>
          </div>
        {/if}

        {#if research}
          <div class="grp">
            <h3>Última hora</h3>
            <dl class="stats">
              {@render kv('Leituras na Steam', research.reads_1h)}
              {@render kv('Exploração', `${research.exploratory_1h} de ${research.exploratory_limit}`, `Teto de ${research.exploratory_limit}, não é cota garantida. ${research.available} vagas livres.`)}
              {@render kv('Acompanhamento', research.followup_1h)}
              {@render kv('CSFloat', research.csfloat_1h)}
              {@render kv('Respostas 429', research.limited_1h)}
              {@render kv('Erros', research.failed_1h)}
            </dl>
          </div>
        {/if}

        {#if trial || exploration || experiment}
          <div class="grp">
            <h3>Exploração</h3>
            <dl class="stats">
              {#if trial}
                <div><dt>Fase</dt><dd>{trialState(trial.phase)}</dd><small>{trial.block_id != null ? `bloco ${trial.block_id} · ` : ''}referência: {policyName(trial.base_policy)}</small></div>
              {/if}
              {#if exploration}
                <div><dt>Candidata</dt><dd>{exploration.candidate ?? 'nenhuma'}</dd><small>{exploration.visits} visitas · {exploration.targets_tested} alvos · {exploration.unique_finds} achados únicos{exploration.retained ? ' · mantida' : ''}</small></div>
              {/if}
              {#if experiment}
                <div><dt>Comparação</dt><dd>{conclusionName(experiment.conclusion)}</dd><small>{experiment.pairs} {experiment.pairs === 1 ? 'par' : 'pares'} de blocos</small></div>
              {/if}
            </dl>
            {#if trial?.reason}<p class="muted why">{trial.reason}</p>{/if}
            {#if experiment?.reason}<p class="muted why">{experiment.reason}</p>{/if}
          </div>
        {/if}

        {#if cohorts.length || wk}
          <div class="grp">
            <h3>Resultados</h3>
            {#if cohorts.length}<div class="scroll">
              <table>
                <thead><tr><th>Grupo</th><th>Registros</th><th>Maduros</th><th>Pendentes</th><th>Sem resultado</th><th>Rotulados</th><th>Fechados</th><th>Lucro previsto</th><th>Lucro virtual</th></tr></thead>
                <tbody>
                  {#each cohorts as c (`${c.policy ?? ''}|${c.kind}`)}
                    <tr><td>{policyName(c.policy)} · {kindName(c.kind)}</td><td>{c.total}</td><td>{c.mature}</td><td>{c.waiting}</td><td>{c.unlabeled}</td><td>{c.labeled}</td>
                      <td title={c.indirect ? `${c.indirect} por indício indireto` : undefined}>{c.closed}</td><td>{money(c.predicted_cents)}</td><td>{c.labeled ? signed(c.virtual_cents) : '—'}</td></tr>
                  {/each}
                </tbody>
              </table>
            </div>{/if}
            {#if wk}
              <div class="scroll">
                <table>
                  <thead><tr><th>Semana</th><th>Achados</th><th>Quase</th><th>Consultas</th><th>Previsto alcançado em 48 h</th><th>Lucro virtual</th></tr></thead>
                  <tbody>
                    {#each wk as w, i}
                      {#if i === 0 || w.queries}
                        <tr><td>{i === 0 ? 'Esta' : 'Anterior'}</td><td>{w.finds}</td><td>{w.near}</td><td>{w.queries}</td><td>{of(w.alerts_closed, w.alerts)}</td><td>{w.labeled ? `${signed(w.profit_cents)} em ${w.labeled}` : '—'}</td></tr>
                      {/if}
                    {/each}
                  </tbody>
                </table>
              </div>
            {/if}
            <p class="muted why">Previsto e virtual são estimativas, não vendas reais.</p>
          </div>
        {/if}

        {#if days.length}
          <div class="grp">
            <h3>Anúncios por dia</h3>
            <div class="scroll">
              <table>
                <thead><tr>
                  <th>Dia</th>
                  <th title="Leituras de anúncios · leituras do livro de ofertas">Consultas · livro</th>
                  <th title="Anúncios vistos pela primeira vez">Novos</th>
                  <th title="Idade mediana do anúncio quando o app o viu (só os de idade conhecida, entre parênteses)">Idade ao ver</th>
                  <th title="Sumiram sem o mesmo item voltar à venda · relistados pelo vendedor">Sumiram · relistados</th>
                  <th title="Quanto o anúncio que sumiu ficou à venda, na mediana">Duram</th>
                  <th title="Quase deram lucro · deram lucro">Quase · achados</th>
                </tr></thead>
                <tbody>
                  {#each days as d (d.day)}
                    <tr><td>{label(d.day)}</td><td>{d.queries} · {d.books}</td><td>{d.new_listings}</td><td>{min(d.age_median_min)} <small class="muted">({d.aged})</small></td>
                      <td>{d.sales} · {d.relists}</td><td>{min(d.life_median_min)}</td><td>{d.near} · <b class:up={d.finds > 0}>{d.finds}</b></td></tr>
                  {/each}
                </tbody>
              </table>
            </div>
          </div>
        {/if}
      </div>
    </details>
  {/if}
</section>

<style>
  section { display: flex; flex-direction: column; gap: 20px; }
  .today { color: var(--muted); }
  .today b { color: var(--ink); }
  .today b.up { color: var(--green); }
  .amber { color: var(--amber); }
  .more { border-top: 1px solid var(--line); }
  .more > summary { cursor: pointer; padding: 14px 0; font-weight: 600; }
  .groups { display: flex; flex-direction: column; gap: 28px; padding-bottom: 8px; }
  .grp { display: flex; flex-direction: column; gap: 10px; }
  h3 { font-size: 13px; font-weight: 600; color: var(--muted); }
  .why { font-size: 13px; max-width: 75ch; }
  .scroll { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; white-space: nowrap; text-align: right; font-size: 13px; }
  th, td { padding: 8px 12px; border-bottom: 1px solid var(--line); }
  th { color: var(--muted); font-weight: 500; font-size: 12px; }
  th:first-child, td:first-child { text-align: left; padding-left: 0; }
  th:last-child, td:last-child { padding-right: 0; }
</style>
