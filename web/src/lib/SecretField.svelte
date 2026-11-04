<script lang="ts">
  import type { KeyStatus } from './types';

  let { label, hint, masked, status, onsave, onclear, plain = false }: {
    label: string; hint?: string; masked: string | null; status?: KeyStatus;
    onsave: (v: string) => Promise<void>; onclear: () => Promise<void>; plain?: boolean;
  } = $props();

  let draft = $state('');
  let busy = $state(false);
  let error = $state('');
  const id = `f-${Math.random().toString(36).slice(2, 8)}`;

  async function run(fn: () => Promise<void>, after?: () => void) {
    busy = true;
    error = '';
    try {
      await fn();
      after?.();
    } catch (e) {
      error = e instanceof Error ? e.message : 'Não consegui salvar.';
    } finally {
      busy = false;
    }
  }
</script>

<div class="field">
  <label for={id}>{label}</label>
  {#if hint}<p class="hint">{hint}</p>{/if}
  <div class="state">
    {#if masked}<code class="mask">{masked}</code>{:else}<span class="muted">Nada salvo</span>{/if}
    {#if status}
      {#if !status.set}<span class="pill mute">Não configurada</span>
      {:else if status.ok === true}<span class="pill ok">Funcionando</span>
      {:else if status.ok === false}<span class="pill bad">Recusada</span>
      {:else}<span class="pill mute">Salva, ainda não testada</span>{/if}
      {#if status.detail}<span class="detail {status.ok === false ? 'down' : 'muted'}">{status.detail}</span>{/if}
    {/if}
  </div>
  <form onsubmit={(e) => { e.preventDefault(); if (draft.trim()) void run(() => onsave(draft.trim()), () => (draft = '')); }}>
    <input {id} type={plain ? 'text' : 'password'} bind:value={draft} placeholder="Cole aqui para trocar" autocomplete="off" spellcheck="false" />
    <button class="btn primary" disabled={busy || !draft.trim()}>Salvar</button>
    <button type="button" class="btn danger" disabled={busy || !masked} onclick={() => run(onclear)}>Apagar</button>
  </form>
  {#if error}<p class="down" role="alert">{error}</p>{/if}
</div>

<style>
  .field { display: flex; flex-direction: column; gap: 6px; }
  label { font-weight: 600; }
  .hint { color: var(--muted); font-size: 13px; }
  .state { display: flex; flex-wrap: wrap; gap: 6px 10px; align-items: center; }
  .mask { font-family: inherit; letter-spacing: 0.04em; color: var(--muted); }
  .detail { font-size: 12px; }
  form { display: flex; gap: 8px; }
  input { flex: 1; min-width: 0; min-height: 36px; padding: 0 10px; border-radius: var(--radius); background: var(--bg); border: 1px solid var(--line); font-size: 14px; }
</style>
