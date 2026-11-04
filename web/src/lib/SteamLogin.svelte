<script lang="ts">
  import { onDestroy } from 'svelte';
  import { api } from './api';
  import { load } from './store.svelte';
  import type { KeyStatus } from './types';

  let { status }: { status?: KeyStatus } = $props();

  type Phase = 'idle' | 'waiting' | 'done' | 'expired' | 'error';
  let phase = $state<Phase>('idle');
  let svg = $state('');
  let account = $state('');
  let error = $state('');
  let timer: ReturnType<typeof setTimeout> | undefined;

  const stop = () => clearTimeout(timer);
  onDestroy(stop);

  async function begin() {
    stop();
    error = '';
    try {
      const q = await api<{ qr_svg: string }>('/api/steam/login/qr', 'POST');
      svg = q.qr_svg;
      phase = 'waiting';
      timer = setTimeout(check, 3000);
    } catch (e) {
      phase = 'error';
      error = e instanceof Error ? e.message : 'Não consegui pedir o QR.';
    }
  }

  async function check() {
    try {
      const s = await api<{ state: Phase; account?: string; qr_svg?: string; detail?: string }>('/api/steam/login/status');
      if (s.qr_svg) svg = s.qr_svg;
      if (s.state === 'waiting') {
        timer = setTimeout(check, 3000);
        return;
      }
      phase = s.state;
      account = s.account ?? '';
      if (s.state === 'done') void load();
    } catch {
      timer = setTimeout(check, 5000); // sem conexão agora; tenta de novo
    }
  }
</script>

<div class="field">
  <h4>Login da Steam</h4>
  <p class="hint">Deixa o Skin Sniper ler seu saldo e comprar. Você entra pelo app da Steam no celular; sua senha não passa por aqui e o login se renova sozinho.</p>
  <div class="state">
    {#if !status || !status.set}<span class="pill mute">Não configurado</span>
    {:else if status.ok === true}<span class="pill ok">Funcionando</span>
    {:else if status.ok === false}<span class="pill bad">Precisa entrar de novo</span>
    {:else}<span class="pill mute">Salvo, ainda não testado</span>{/if}
    {#if status?.detail}<span class="detail {status.ok === false ? 'down' : 'muted'}">{status.detail}</span>{/if}
  </div>

  {#if phase === 'waiting'}
    <div class="qr">
      <div class="code" role="img" aria-label="QR code para entrar com o app da Steam">{@html svg}</div>
      <ol>
        <li>Abra o app <b>Steam</b> no celular.</li>
        <li>Toque no ícone do escudo (<b>Steam Guard</b>).</li>
        <li>Escolha <b>Escanear um QR code</b> e aponte para a imagem.</li>
        <li>Confirme no celular. Esta tela atualiza sozinha.</li>
      </ol>
    </div>
  {:else if phase === 'done'}
    <p class="ok-msg" role="status">Conectado{account ? ` como ${account}` : ''}. Pronto, pode fechar.</p>
  {:else if phase === 'expired'}
    <p class="down" role="alert">O QR venceu. Peça outro e escaneie logo.</p>
  {:else if phase === 'error'}
    <p class="down" role="alert">{error || 'A Steam recusou o login. Tente de novo.'}</p>
  {/if}

  <div class="actions">
    <button class="btn primary" onclick={begin} disabled={phase === 'waiting' && !svg}>
      {phase === 'waiting' ? 'Gerar outro QR' : 'Entrar com o app da Steam'}
    </button>
  </div>
</div>

<style>
  .field { display: flex; flex-direction: column; gap: 8px; }
  h4 { font-weight: 600; font-size: 14px; }
  .hint { color: var(--muted); font-size: 13px; }
  .state { display: flex; flex-wrap: wrap; gap: 6px 10px; align-items: center; }
  .detail { font-size: 12px; }
  .qr { display: flex; gap: 16px; align-items: flex-start; flex-wrap: wrap; }
  .code { width: 200px; height: 200px; background: #fff; padding: 6px; border-radius: var(--radius); }
  .code :global(svg) { width: 100%; height: 100%; }
  ol { margin: 0; padding-left: 18px; display: flex; flex-direction: column; gap: 6px; font-size: 13px; flex: 1; min-width: 180px; }
  .ok-msg { color: var(--green); }
  .actions { display: flex; gap: 8px; }
</style>
