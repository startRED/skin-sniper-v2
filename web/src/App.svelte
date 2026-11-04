<script lang="ts">
  import { onMount } from 'svelte';
  import { app, load, start } from './lib/store.svelte';
  import TopBar from './lib/TopBar.svelte';
  import ComboBar from './lib/ComboBar.svelte';
  import OppTable from './lib/OppTable.svelte';
  import Purchases from './lib/Purchases.svelte';
  import SettingsPanel from './lib/SettingsPanel.svelte';

  onMount(start);

  const count = $derived(Object.keys(app.opps).length);
  const noSaldo = $derived(app.state?.saldo_source === 'none');
  const noSteam = $derived(app.state != null && !app.state.keys.steam.set);
</script>

<TopBar />

{#if app.conn === 'down'}
  <div class="banner" role="status">
    Sem conexão com o Skin Sniper. Estamos tentando de novo — enquanto isso a lista abaixo pode estar desatualizada e não dá para comprar.
  </div>
{/if}

<main>
  {#if app.loadError}
    <div class="empty" role="alert">
      <h2>Não consegui abrir o painel</h2>
      <p>{app.loadError}</p>
      <button class="btn primary" onclick={load}>Tentar de novo</button>
    </div>
  {:else if !app.loaded}
    <div class="empty" role="status"><p>Carregando as oportunidades…</p></div>
  {:else}
    <ComboBar />

    <section aria-labelledby="h-opp">
      <div class="title">
        <h2 id="h-opp">Oportunidades</h2>
        <p class="muted">
          {#if count > 0}{count} {count === 1 ? 'skin dá' : 'skins dão'} lucro depois das taxas e {count === 1 ? 'cabe' : 'cabem'} no seu saldo.
            <b class="up">Seguro</b> = ainda dá lucro vendendo pelo preço rápido.{/if}
        </p>
      </div>

      {#if count === 0}
        <div class="empty" role="status">
          {#if noSaldo}
            <h3>Falta o seu saldo</h3>
            <p>Digite quanto você tem para gastar (lá em cima, em “Saldo”) ou conecte a Steam em Configurações. Com isso eu sei quais skins cabem.</p>
          {:else}
            <h3>Nenhuma skin com lucro agora</h3>
            <p>Continuamos vigiando. Quando aparecer uma que dê lucro depois das taxas e caiba no seu saldo, ela entra aqui sozinha{app.settings?.sound ? ' e você ouve um aviso' : ''}.</p>
          {/if}
          {#if noSteam}
            <p>Dica: conecte a sua conta da Steam para eu ler o saldo e poder comprar.</p>
            <button class="btn" onclick={() => (app.settingsOpen = true)}>Abrir configurações</button>
          {/if}
        </div>
      {:else}
        <OppTable />
      {/if}
    </section>

    <Purchases />
  {/if}
</main>

<SettingsPanel />
<div class="sr-only" aria-live="polite" role="status">{app.announce}</div>

<style>
  main { padding: 16px 24px 80px; max-width: 1760px; margin: 0 auto; display: flex; flex-direction: column; gap: 16px; }
  .banner { background: var(--red-bg); color: var(--red); padding: 8px 24px; border-bottom: 1px solid var(--red); }
  .title { display: flex; align-items: baseline; gap: 16px; flex-wrap: wrap; margin-bottom: 8px; }
  h2 { font-size: 15px; }
  .empty { background: var(--panel); border-radius: var(--radius); padding: 40px 24px; display: flex; flex-direction: column; gap: 10px; align-items: flex-start; }
  .empty p, .empty h3, .empty h2 { max-width: 62ch; }
  .empty h3, .empty h2 { font-size: 17px; }
  .empty p { color: var(--muted); }
  @media (max-width: 899px) {
    main { padding: 12px 12px 60px; }
    .banner { padding: 8px 14px; }
  }
</style>
