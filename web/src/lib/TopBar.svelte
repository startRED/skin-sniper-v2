<script lang="ts">
  import { app, bookLane, readingSaldo, setSaldo, steamBlocked } from './store.svelte';
  import { ago, centsToInput, clock, money, parseBRL } from './format';

  let editing = $state(false);
  let draft = $state('');
  let error = $state('');
  let busy = $state(false);

  const st = $derived(app.state);
  const paused = $derived(st?.hunt.paused_until != null && st.hunt.paused_until > app.now);
  const sourceLabel = $derived(
    st?.saldo_source === 'steam' ? 'lido da Steam' : st?.saldo_source === 'manual' ? 'digitado por você'
    : readingSaldo() ? 'lendo da Steam…' : 'sem saldo'
  );

  function edit() {
    draft = st && st.saldo_source !== 'none' ? centsToInput(st.saldo_cents) : '';
    error = '';
    editing = true;
  }

  async function save(cents: number | null) {
    busy = true;
    error = '';
    try {
      await setSaldo(cents);
      editing = false;
    } catch (e) {
      error = e instanceof Error ? e.message : 'Não consegui salvar.';
    } finally {
      busy = false;
    }
  }

  function submit(e: SubmitEvent) {
    e.preventDefault();
    const c = parseBRL(draft);
    if (c == null) error = 'Digite um valor como 36,08.';
    else void save(c);
  }

  function focusOnMount(el: HTMLInputElement) {
    el.focus();
    el.select();
  }
</script>

<header class="top">
  <h1 class="brand">Skin Sniper</h1>

  <div class="saldo">
    <span class="label">Saldo</span>
    {#if editing}
      <form onsubmit={submit} class="edit">
        <label class="sr-only" for="saldo-in">Saldo em reais</label>
        <span class="rs">R$</span>
        <input id="saldo-in" bind:value={draft} inputmode="decimal" autocomplete="off" use:focusOnMount
          onkeydown={(e) => e.key === 'Escape' && (editing = false)} />
        <button class="btn primary" disabled={busy}>Salvar</button>
        {#if st?.saldo_source === 'manual'}
          <button type="button" class="btn" disabled={busy} onclick={() => save(null)}>Ler da Steam</button>
        {/if}
        <button type="button" class="btn" onclick={() => (editing = false)}>Cancelar</button>
      </form>
      {#if error}<span class="err" role="alert">{error}</span>{/if}
    {:else}
      <button class="val" onclick={edit} disabled={!st} title="Clique para digitar outro saldo">
        {st ? (st.saldo_source === 'none' ? 'Informar saldo' : money(st.saldo_cents)) : '—'}
        <svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true"><path d="M2 11.5V14h2.5l7-7L9 4.5l-7 7ZM13.7 4.8a.7.7 0 0 0 0-1l-1.5-1.5a.7.7 0 0 0-1 0l-1.2 1.2L12.5 6l1.2-1.2Z" fill="currentColor"/></svg>
      </button>
      {#if st}<span class="pill {st.saldo_source === 'steam' ? 'ok' : st.saldo_source === 'manual' ? 'warn' : 'mute'}">{sourceLabel}</span>{/if}
    {/if}
  </div>

  <div class="hunt" class:paused>
    <span class="dot" class:live={app.conn === 'live' && !paused} class:warn={paused} class:off={app.conn === 'down'}></span>
    <div>
      {#if app.conn === 'down'}
        <span>Desconectado</span>
      {:else if !st}
        <span>Conectando…</span>
      {:else if bookLane()}
        <span>Consultando preços · leitura de anúncios em pausa{st.hunt.last_ok_at ? ' ' + ago(st.hunt.last_ok_at, app.now) : ''} ·
          tenta de novo às {clock(st.hunt.paused_until!)}</span>
      {:else if steamBlocked()}
        <span>Caça em pausa{st.hunt.last_ok_at ? ' ' + ago(st.hunt.last_ok_at, app.now) : ''} · a Steam recusou as consultas ·
          tenta de novo às {clock(st.hunt.paused_until!)}</span>
      {:else if paused}
        <span>Em pausa · a Steam pediu calma · volta em {st.hunt.paused_until! - app.now} s</span>
      {:else if st.hunt.watching === 0}
        <span>Escolhendo o que caçar…</span>
      {:else}
        <span>Caçando <b>{st.hunt.watching}</b> skins · {st.hunt.queries_per_min} consultas totais/min ·
          {st.hunt.last_query_at ? ago(st.hunt.last_query_at, app.now) : 'ainda sem consulta'}</span>
      {/if}
      {#if app.conn !== 'down' && st?.hunt.message && !(paused && st.hunt.message.includes('429')) && !readingSaldo()}<span class="msg">{st.hunt.message}</span>{/if}
    </div>
  </div>

  <button class="btn cfg" onclick={() => (app.settingsOpen = true)} aria-label="Abrir configurações">
    <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"><path d="M2 4.5h6M12 4.5h2M2 11.5h2M8 11.5h6"/><circle cx="10" cy="4.5" r="1.8"/><circle cx="6" cy="11.5" r="1.8"/></svg>
    <span class="cfg-t">Configurações</span>
  </button>
</header>

<style>
  .top {
    display: flex; align-items: center; gap: 28px; padding: 12px 24px;
    background: var(--panel); border-bottom: 1px solid var(--line);
    position: sticky; top: 0; z-index: 10;
  }
  .brand { font-size: 16px; font-weight: 700; letter-spacing: -0.01em; white-space: nowrap; }
  .saldo { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .label { color: var(--muted); }
  .val {
    display: inline-flex; align-items: center; gap: 8px; background: none; border: 1px solid transparent;
    border-radius: var(--radius); padding: 2px 8px; margin: -2px -8px; font-size: 22px; font-weight: 700;
    letter-spacing: -0.01em;
  }
  .val:hover:not(:disabled) { border-color: var(--line); background: var(--panel-2); }
  .val svg { color: var(--faint); }
  .edit { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .edit .rs { color: var(--muted); }
  input {
    width: 110px; min-height: 36px; padding: 0 10px; border-radius: var(--radius);
    background: var(--bg); border: 1px solid var(--line); font-size: 16px;
  }
  .err { color: var(--red); }
  .hunt { display: flex; align-items: center; gap: 10px; margin-left: auto; color: var(--muted); min-width: 0; }
  .hunt b { color: var(--ink); }
  .hunt > div { display: flex; flex-direction: column; }
  .msg { font-size: 12px; color: var(--muted); }
  .paused { color: var(--amber); }
  .dot { width: 9px; height: 9px; border-radius: 50%; background: var(--faint); flex: none; }
  .dot.live { background: var(--green); animation: pulse 2.4s ease-in-out infinite; }
  .dot.warn { background: var(--amber); }
  .dot.off { background: var(--red); }
  @keyframes pulse { 50% { opacity: 0.35; } }

  @media (max-width: 899px) {
    .top { flex-wrap: wrap; gap: 8px 14px; padding: 10px 14px; position: static; }
    .brand { order: 1; }
    .cfg { order: 2; margin-left: auto; }
    .saldo { order: 3; width: 100%; }
    .hunt { order: 4; width: 100%; margin-left: 0; font-size: 13px; }
    .cfg-t { display: none; }
    .cfg { width: 40px; padding: 0; }
  }
</style>
