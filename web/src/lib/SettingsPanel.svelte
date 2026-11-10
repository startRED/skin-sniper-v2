<script lang="ts">
  import { app, notify, saveSettings } from './store.svelte';
  import { centsToInput, parseBRL } from './format';
  import { unlockAudio } from './sound';
  import SecretField from './SecretField.svelte';
  import type { Opportunity, SettingsInput } from './types';

  let dlg: HTMLDialogElement;
  let perm = $state(typeof Notification === 'undefined' ? 'unsupported' : Notification.permission);
  let minDraft = $state('');
  let panelDraft = $state('');
  let msg = $state('');
  $effect(() => {
    if (app.settingsOpen && !dlg.open) {
      minDraft = centsToInput(app.settings?.notify_min_profit_cents ?? 100);
      panelDraft = app.settings?.panel_url ?? '';
      msg = '';
      dlg.showModal();
    } else if (!app.settingsOpen && dlg.open) dlg.close();
  });

  const s = $derived(app.settings);
  const keys = $derived(app.state?.keys);
  const save = (patch: Partial<SettingsInput>) => saveSettings(patch);

  async function ask() {
    if (typeof Notification !== 'undefined') perm = await Notification.requestPermission();
  }
  async function patch(p: Partial<SettingsInput>, ok: string) {
    msg = '';
    try {
      await saveSettings(p);
      msg = ok;
    } catch (e) {
      msg = e instanceof Error ? e.message : 'Não consegui salvar.';
    }
  }
  function saveMin() {
    const c = parseBRL(minDraft);
    if (c == null) msg = 'Digite um valor como 1,00.';
    else void patch({ notify_min_profit_cents: c }, 'Lucro mínimo salvo.');
  }
  function test() {
    unlockAudio();
    const o = { id: 'teste', name: 'Teste de aviso', icon_url: '', sell: { mid: { profit_cents: 100, price_cents: 100 } }, price_cents: 0 } as unknown as Opportunity;
    notify(o);
  }
</script>

<dialog bind:this={dlg} onclose={() => (app.settingsOpen = false)} onclick={(e) => e.target === dlg && (app.settingsOpen = false)} aria-labelledby="h-cfg">
  <div class="box">
    <div class="bar">
      <h2 id="h-cfg">Configurações</h2>
      <button class="btn" onclick={() => (app.settingsOpen = false)}>Fechar</button>
    </div>

    {#if !s}
      <p class="muted">Carregando as configurações…</p>
    {:else}
      <section>
        <h3>Chaves</h3>
        <SecretField label="Chave do CSFloat" masked={s.csfloat_key} status={keys?.csfloat ?? null}
          hint="Ajuda a avaliar skins raras com vendas de verdade."
          onsave={(v) => save({ csfloat_key: v })} onclear={() => save({ csfloat_key: null })} />
        <SecretField label="Token do bot do Telegram" masked={s.telegram_token} status={keys?.telegram ?? null}
          hint="Para receber os avisos no celular, mesmo com o painel fechado."
          onsave={(v) => save({ telegram_token: v })} onclear={() => save({ telegram_token: null })} />
        <SecretField label="Chat do Telegram (número)" masked={s.telegram_chat_id} plain
          onsave={(v) => save({ telegram_chat_id: v })} onclear={() => save({ telegram_chat_id: null })} />
      </section>

      <section>
        <h3>Endereço do painel</h3>
        <p class="hint">O endereço que abre no celular, para os links do Telegram. Ex.: http://192.168.0.10:8788.</p>
        <form class="inline" onsubmit={(e) => { e.preventDefault(); void patch({ panel_url: panelDraft.trim() || null }, 'Endereço salvo.'); }}>
          <label class="sr-only" for="panel-url">Endereço do painel</label>
          <input id="panel-url" bind:value={panelDraft} placeholder="http://…" autocomplete="off" />
          <button class="btn primary">Salvar</button>
        </form>
      </section>

      <section>
        <h3>Avisos</h3>
        <label class="switch">
          <input type="checkbox" checked={s.sound} onchange={(e) => patch({ sound: e.currentTarget.checked }, e.currentTarget.checked ? 'Som ligado.' : 'Som desligado.')} />
          <span>Tocar som nas oportunidades</span>
        </label>
        <form class="inline" onsubmit={(e) => { e.preventDefault(); saveMin(); }}>
          <label for="min">Lucro mínimo para avisar</label>
          <span class="rs">R$</span>
          <input id="min" bind:value={minDraft} inputmode="decimal" autocomplete="off" style="max-width:100px" />
          <button class="btn primary">Salvar</button>
        </form>
        <div class="inline">
          {#if perm === 'granted'}
            <span class="pill ok">Avisos do navegador ligados</span>
          {:else if perm === 'denied'}
            <span class="pill bad">Avisos bloqueados no navegador</span>
            <span class="hint">Libere nas configurações do site (cadeado ao lado do endereço).</span>
          {:else if perm === 'unsupported'}
            <span class="hint">Este navegador não mostra avisos.</span>
          {:else}
            <button class="btn" onclick={ask}>Ligar avisos do navegador</button>
          {/if}
          <button class="btn" onclick={test}>Testar aviso</button>
        </div>
      </section>
      <p class="msg" role="status">{msg}</p>
    {/if}
  </div>
</dialog>

<style>
  dialog {
    margin: 0 0 0 auto; height: 100dvh; max-height: 100dvh; width: min(480px, 100vw); padding: 0;
    background: var(--panel); color: var(--ink); border: 0; border-left: 1px solid var(--line);
  }
  dialog::backdrop { background: rgba(0, 0, 0, 0.55); }
  .box { padding: 20px 24px 40px; display: flex; flex-direction: column; gap: 24px; }
  .bar { display: flex; justify-content: space-between; align-items: center; position: sticky; top: 0; z-index: 1; background: var(--panel); padding: 4px 0; }
  h2 { font-size: 18px; }
  h3 { font-size: 13px; color: var(--muted); font-weight: 600; margin-bottom: 4px; }
  section { display: flex; flex-direction: column; gap: 16px; }
  .hint { color: var(--muted); font-size: 13px; }
  .inline { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .inline input:not([type='checkbox']) { flex: 1; min-width: 0; }
  input:not([type='checkbox']) { min-height: 36px; padding: 0 10px; border-radius: var(--radius); background: var(--bg); border: 1px solid var(--line); font-size: 14px; }
  .switch { display: flex; gap: 10px; align-items: center; }
  .switch input { width: 18px; height: 18px; accent-color: var(--green); }
  .msg { min-height: 20px; color: var(--green); }
  @media (max-width: 899px) { dialog { width: 100vw; border-left: 0; } .box { padding: 16px 16px 40px; } }
</style>
