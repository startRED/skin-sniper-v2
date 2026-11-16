<script lang="ts">
  import { api } from './api';
  import { money } from './format';
  import type { AlertReport, NoticeTest } from './types';
  let { report }: { report: AlertReport } = $props();
  let test = $state<NoticeTest | null>(null);
  let busy = $state(false);
  let error = $state('');
  let shown = $state(20);
  const current = $derived(test ?? report.test);
  const date = (at: number) => new Date(at * 1000).toLocaleString('pt-BR');
  async function send(appeared?: boolean) {
    if (busy) return;
    busy = true;
    error = '';
    try {
      test = appeared === undefined
        ? await api<NoticeTest>('/api/learning/test', 'POST')
        : await api<NoticeTest>('/api/learning/test/answer', 'POST', { id: current?.id, appeared });
    } catch (e) { error = e instanceof Error ? e.message : 'Não consegui salvar o teste.'; }
    finally { busy = false; }
  }
</script>

<section aria-labelledby="alert-title">
  <div class="heading"><h3 id="alert-title">O que aconteceu com cada aviso</h3><button class="btn" disabled={busy} onclick={() => send()}>{busy ? 'Aguarde…' : 'Testar aviso'}</button></div>
  <p class="muted">O Windows aceitar o pedido não prova que o aviso apareceu. Sair do ar pode ser compra ou retirada; não confirma venda nem lucro. As vendas do grupo são as que ficaram no registro. Preços em dólar usam a proporção com o preço comum e a base em reais gravada no aviso.</p>
  <div aria-live="polite">
    {#if current}
      <p>Teste de {date(current.at)}: {current.state}{current.reason ? ` · ${current.reason}` : ''}.</p>
      {#if current.appeared === null && current.state !== 'pendente'}
        <div class="answer"><span>O aviso apareceu no PC?</span><button class="btn" disabled={busy} onclick={() => send(true)}>apareceu</button><button class="btn" disabled={busy} onclick={() => send(false)}>não apareceu</button></div>
      {:else if current.answered_at}
        <p>Resposta salva em {date(current.answered_at)}: {current.appeared ? 'apareceu' : 'não apareceu'}.</p>
      {/if}
    {/if}
    {#if error}<p role="alert" class="amber">{error}</p>{/if}
  </div>
  {#if !report.rows.length}<p class="muted">Nenhum aviso registrado ainda.</p>{/if}
  <ol class="alerts">
    {#each report.rows.slice(0, shown) as row (row.id)}
      <li><div class="heading"><h4>{row.name}</h4><span>{money(row.price_cents)}</span></div>
        <p class="muted">{date(row.at)} · anúncio {row.id}{#if row.above_saldo}<span class="amber"> · acima do saldo</span>{/if}</p>
        <dl>{#if !row.above_saldo}<div><dt>Windows</dt><dd>{row.windows}</dd></div>{/if}{#if row.above_saldo || !row.telegram.includes('Telegram sem token ou chat id')}<div><dt>Telegram</dt><dd>{row.telegram}</dd></div>{/if}<div><dt>Anúncio</dt><dd>{row.listing}</dd></div><div><dt>Conferência de 48 h</dt><dd>{row.group_check}</dd></div><div><dt>Preço do grupo</dt><dd>{row.price_check}{row.sales ? ` · ${row.sales} vendas registradas` : ''}</dd></div></dl>
      </li>
    {/each}
  </ol>
  {#if report.rows.length > shown}<button class="btn" onclick={() => shown += 20}>Mostrar mais avisos ({report.rows.length - shown})</button>{/if}
  <h3>Sete números pra conferir a melhora</h3>
  <p class="muted">Janela de 7 dias desde {date(report.started_at)}. As conferências de 48 h fecham em {date(report.closes_at)}. Só há melhora se os sete forem cumpridos.</p>
  <ol class="metrics">
    {#each report.metrics as metric}
      <li><h4>{metric.label}</h4><p>Base: {metric.baseline}</p><p>{metric.value}</p><p class="muted">{metric.state}</p></li>
    {/each}
  </ol>
</section>

<style>
  section { display: grid; gap: 12px; margin: 12px 0 24px; font-size: 14px; line-height: 1.5; }
  .heading, .answer { display: flex; flex-wrap: wrap; align-items: center; gap: 12px; justify-content: space-between; }
  .answer { justify-content: flex-start; margin-top: 8px; }
  h3 { font-size: 17px; } h4 { font-size: 15px; overflow-wrap: anywhere; }
  .btn { min-height: 40px; }
  .alerts, .metrics { list-style: none; padding: 0; display: grid; gap: 12px; }
  .alerts li, .metrics li { padding: 16px; border: 1px solid var(--line); border-radius: var(--radius); background: var(--panel); min-width: 0; overflow-wrap: anywhere; }
  dl { display: grid; gap: 5px; margin-top: 8px; } dl div { display: grid; grid-template-columns: 160px 1fr; gap: 8px; } dt { color: var(--muted); } dd { margin: 0; }
  .metrics { grid-template-columns: repeat(2, minmax(0,1fr)); }
  @media (max-width: 600px) { .metrics { grid-template-columns: 1fr; } dl div { grid-template-columns: 1fr; gap: 0; } }
</style>
