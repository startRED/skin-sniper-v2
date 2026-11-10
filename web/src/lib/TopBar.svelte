<script lang="ts">
  import { app, bookLane, setSaldo, steamBlocked } from './store.svelte';
  import { ago, centsToInput, clock, money, parseBRL } from './format';

  let { tabs, current }: { tabs: { id: string; label: string; count: number }[]; current: string } = $props();

  let draft = $state('');
  let error = $state('');
  let busy = $state(false);

  const st = $derived(app.state);
  const paused = $derived(st?.hunt.paused_until != null && st.hunt.paused_until > app.now);

  // Abre pelo botão do saldo ou pela tela vazia (app.saldoEdit).
  $effect(() => {
    if (app.saldoEdit) {
      draft = app.state && app.state.saldo_source !== 'none' ? centsToInput(app.state.saldo_cents) : '';
      error = '';
    }
  });

  async function save(cents: number) {
    busy = true;
    error = '';
    try {
      await setSaldo(cents);
      app.saldoEdit = false;
    } catch (e) {
      error = e instanceof Error ? e.message : 'Não consegui salvar.';
    } finally {
      busy = false;
    }
  }

  function submit(e: SubmitEvent) {
    e.preventDefault();
    const c = parseBRL(draft);
    if (c == null || !Number.isSafeInteger(c)) error = 'Digite um valor como 36,08.';
    else void save(c);
  }

  function focusOnMount(el: HTMLInputElement) {
    el.focus();
    el.select();
  }
</script>

<header class="top">
  <h1 class="brand">Skin Sniper</h1>

  <nav class="tabs" aria-label="Seções">
    {#each tabs as t (t.id)}
      <a href="#{t.id}" aria-current={t.id === current ? 'page' : undefined}>{t.label}{#if t.count > 0}<span class="n">{t.count}</span>{/if}</a>
    {/each}
  </nav>

  <div class="hunt" class:paused>
    <span class="dot" class:live={app.conn === 'live' && !paused} class:warn={paused} class:off={app.conn === 'down'}></span>
    <div>
      {#if app.conn === 'down'}
        <span>Desconectado</span>
      {:else if !st}
        <span>Conectando…</span>
      {:else if bookLane()}
        <span>Só preços · anúncios voltam às {clock(st.hunt.paused_until!)}</span>
      {:else if steamBlocked()}
        <span>Em pausa · volta às {clock(st.hunt.paused_until!)}</span>
      {:else if paused}
        <span>Em pausa · volta em {st.hunt.paused_until! - app.now} s</span>
      {:else if st.hunt.watching === 0}
        <span>Escolhendo o que caçar…</span>
      {:else}
        <span>Caçando <b>{st.hunt.watching}</b> skins · {st.hunt.queries_per_min} consultas totais/min ·
          {st.hunt.last_query_at ? ago(st.hunt.last_query_at, app.now) : 'ainda sem consulta'}</span>
      {/if}
      {#if app.conn !== 'down' && st?.hunt.message && !(paused && st.hunt.message.includes('429'))}<span class="msg" title={st.hunt.message}>{st.hunt.message}</span>{/if}
    </div>
  </div>

  <div class="saldo">
    {#if app.saldoEdit}
      <form onsubmit={submit} class="edit">
        <label for="saldo-in">Saldo R$</label>
        <input id="saldo-in" bind:value={draft} inputmode="decimal" autocomplete="off" use:focusOnMount
          aria-invalid={!!error} aria-describedby={error ? 'saldo-err' : undefined}
          onkeydown={(e) => e.key === 'Escape' && (app.saldoEdit = false)} />
        <button class="btn primary" disabled={busy}>Salvar</button>
        <button type="button" class="btn" onclick={() => (app.saldoEdit = false)}>Cancelar</button>
      </form>
      {#if error}<span class="err" id="saldo-err" role="alert">{error}</span>{/if}
    {:else}
      <span class="label">Saldo</span>
      <button class="val" class:none={st?.saldo_source === 'none'} onclick={() => (app.saldoEdit = true)} disabled={!st} title="Trocar o saldo">
        {st ? (st.saldo_source === 'none' ? 'Informar' : money(st.saldo_cents)) : '—'}
        <svg width="13" height="13" viewBox="0 0 16 16" aria-hidden="true"><path d="M2 11.5V14h2.5l7-7L9 4.5l-7 7ZM13.7 4.8a.7.7 0 0 0 0-1l-1.5-1.5a.7.7 0 0 0-1 0l-1.2 1.2L12.5 6l1.2-1.2Z" fill="currentColor"/></svg>
      </button>
    {/if}
  </div>

  <button class="btn cfg" onclick={() => (app.settingsOpen = true)} aria-label="Abrir configurações" title="Configurações">
    <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"><path d="M2 4.5h6M12 4.5h2M2 11.5h2M8 11.5h6"/><circle cx="10" cy="4.5" r="1.8"/><circle cx="6" cy="11.5" r="1.8"/></svg>
  </button>
</header>

<style>
  .top {
    display: flex; align-items: center; gap: 24px; padding: 0 var(--gutter); min-height: var(--top);
    background: var(--panel); border-bottom: 1px solid var(--line);
    position: sticky; top: 0; z-index: 10;
  }
  .brand { font-size: 15px; font-weight: 700; letter-spacing: -0.01em; white-space: nowrap; }

  .tabs { display: flex; align-self: stretch; gap: 4px; }
  .tabs a {
    display: flex; align-items: center; gap: 7px; padding: 0 12px; color: var(--muted); text-decoration: none;
    white-space: nowrap; font-weight: 550; border-bottom: 2px solid transparent; margin-bottom: -1px;
    transition: color 0.15s;
  }
  .tabs a:hover { color: var(--ink); }
  .tabs a[aria-current] { color: var(--ink); border-bottom-color: var(--green); }
  .n {
    min-width: 20px; padding: 0 6px; border-radius: 99px; background: var(--green); color: var(--green-ink);
    font-size: 12px; font-weight: 700; line-height: 20px; text-align: center;
  }

  .hunt { display: flex; align-items: center; gap: 10px; margin-left: auto; color: var(--muted); min-width: 0; font-size: 13px; }
  .hunt b { color: var(--ink); }
  .hunt > div { display: flex; flex-direction: column; min-width: 0; }
  .msg { font-size: 12px; max-width: 58ch; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .paused { color: var(--amber); }
  .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--faint); flex: none; }
  .dot.live { background: var(--green); animation: pulse 2.4s ease-in-out infinite; }
  .dot.warn { background: var(--amber); }
  .dot.off { background: var(--red); }
  @keyframes pulse { 50% { opacity: 0.35; } }

  .saldo { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .label { color: var(--muted); font-size: 13px; }
  .val {
    display: inline-flex; align-items: center; gap: 8px; min-height: 36px; padding: 0 10px;
    background: var(--panel-2); border: 1px solid var(--line); border-radius: var(--radius);
    font-size: 16px; font-weight: 700; letter-spacing: -0.01em;
  }
  .val:hover:not(:disabled) { background: #232b34; }
  .val.none { background: var(--green); border-color: var(--green); color: var(--green-ink); }
  .val.none:hover:not(:disabled) { background: #5be0a4; }
  .val svg { opacity: 0.6; }
  .edit { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .edit label { color: var(--muted); font-size: 13px; }
  input {
    width: 100px; min-height: 36px; padding: 0 10px; border-radius: var(--radius);
    background: var(--bg); border: 1px solid var(--line); font-size: 16px;
  }
  input[aria-invalid='true'] { border-color: var(--red); }
  .err { color: var(--red); font-size: 13px; }
  .cfg { width: 36px; padding: 0; flex: none; }

  @media (max-width: 1239px) {
    .top { flex-wrap: wrap; gap: 0 14px; padding: 8px 14px 0; position: static; }
    .brand { order: 1; }
    .saldo { order: 2; margin-left: auto; }
    .cfg { order: 3; }
    .hunt { order: 4; width: 100%; margin: 8px 0 0; }
    .msg { white-space: normal; max-width: none; }
    .tabs { order: 5; width: calc(100% + 28px); margin: 6px -14px 0; padding: 0 6px; overflow-x: auto; scrollbar-width: none; }
    .tabs a { min-height: 44px; }
  }
</style>
