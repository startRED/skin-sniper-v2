<script lang="ts">
  import { onMount } from 'svelte';
  import { app, load, lostRows, readingSaldo, start, steamBlocked } from './lib/store.svelte';
  import { ago, clock } from './lib/format';
  import TopBar from './lib/TopBar.svelte';
  import ComboBar from './lib/ComboBar.svelte';
  import OppTable from './lib/OppTable.svelte';
  import Watch from './lib/Watch.svelte';
  import Purchases from './lib/Purchases.svelte';
  import SettingsPanel from './lib/SettingsPanel.svelte';

  onMount(start);

  const count = $derived(Object.values(app.opps).filter((o) => !o.gone_at).length);
  const lostCount = $derived(lostRows().length);
  const noSaldo = $derived(app.state?.saldo_source === 'none');
  const noSteam = $derived(app.state != null && !app.state.keys.steam.set);
  const reading = $derived(readingSaldo());
  const blocked = $derived(steamBlocked());
</script>

<TopBar />

{#if app.conn === 'down'}
  <div class="banner" role="status">
    Sem conexão com o Skin Sniper. Estamos tentando de novo — enquanto isso a lista abaixo pode estar desatualizada e não dá para comprar.
  </div>
{/if}

{#if blocked && app.state}
  <div class="warn block" role="status">
    <p><b>A caça está parada{app.state.hunt.last_ok_at ? ' ' + ago(app.state.hunt.last_ok_at, app.now) : ''}: a Steam bloqueou as consultas deste IP.</b>
      Ela limita quantas consultas cada IP faz ao mercado e, passado o limite, barra por algumas horas. Não é erro do app nem da
      sua conta, e reiniciar não libera.</p>
    <p>O app tenta de novo sozinho (próxima tentativa às {clock(app.state.hunt.paused_until!)}) e volta a caçar assim que a Steam
      liberar. Nada se perde.{#if noSaldo} Enquanto isso o saldo não dá para ler: se quiser, digite-o lá em cima.{/if}</p>
  </div>
{/if}

{#if app.warnings.length}
  <div class="warn" role="status">{#each app.warnings as w}<p>{w}</p>{/each}</div>
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
        <div class="empty compact" role="status">
          {#if reading}
            <h3>Lendo seu saldo na Steam…</h3>
            <p>Assim que a Steam responder eu escolho as skins que cabem. Se ela pediu calma, isso espera a pausa lá em cima acabar.</p>
          {:else if noSaldo}
            <h3>Falta o seu saldo</h3>
            <p>Digite quanto você tem para gastar (lá em cima, em “Saldo”) ou conecte a Steam em Configurações. Com isso eu sei quais skins cabem.</p>
          {:else}
            <h3>Nenhuma skin com lucro agora.</h3>
          {/if}
          {#if noSteam}
            <p>Dica: conecte a sua conta da Steam para eu ler o saldo e poder comprar.</p>
            <button class="btn" onclick={() => (app.settingsOpen = true)}>Abrir configurações</button>
          {/if}
        </div>
      {:else}
        <OppTable />
      {/if}
      {#if lostCount > 0}
        <h3 class="lost">Perdidas nas últimas 24 h <span class="muted">· {lostCount} · saíram da lista; ficam aqui só para você ver o que escapou</span></h3>
        <OppTable lost />
      {/if}
      <Watch collapsed={count > 0} />
    </section>

    <Purchases />
  {/if}
</main>

<SettingsPanel />
<div class="sr-only" aria-live="polite" role="status">{app.announce}</div>

<style>
  main { padding: 16px 24px 80px; max-width: 1760px; margin: 0 auto; display: flex; flex-direction: column; gap: 16px; }
  .banner { background: var(--red-bg); color: var(--red); padding: 8px 24px; border-bottom: 1px solid var(--red); }
  .warn { background: var(--amber-bg); color: var(--amber); padding: 8px 24px; border-bottom: 1px solid var(--amber); }
  .block { display: flex; flex-direction: column; gap: 4px; padding-block: 10px; }
  .block p { max-width: 110ch; }
  .block b { color: var(--ink); }
  .title { display: flex; align-items: baseline; gap: 16px; flex-wrap: wrap; margin-bottom: 8px; }
  h2 { font-size: 15px; }
  .lost { font-size: 14px; margin: 20px 0 8px; }
  .lost .muted { font-weight: 400; font-size: 13px; }
  .empty { background: var(--panel); border-radius: var(--radius); padding: 40px 24px; display: flex; flex-direction: column; gap: 10px; align-items: flex-start; }
  .empty p, .empty h3, .empty h2 { max-width: 62ch; }
  .empty h3, .empty h2 { font-size: 17px; }
  .empty p { color: var(--muted); }
  .empty.compact { padding: 12px 16px; gap: 6px; }
  .empty.compact h3 { font-size: 14px; }
  @media (max-width: 899px) {
    main { padding: 12px 12px 60px; }
    .banner, .warn { padding: 8px 14px; }
  }
</style>
