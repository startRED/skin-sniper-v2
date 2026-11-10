<script lang="ts">
  import { onMount } from 'svelte';
  import { app, bookLane, load, lostRows, start, steamBlocked } from './lib/store.svelte';
  import { ago, clock } from './lib/format';
  import TopBar from './lib/TopBar.svelte';
  import ComboBar from './lib/ComboBar.svelte';
  import OppTable from './lib/OppTable.svelte';
  import Watch from './lib/Watch.svelte';
  import Purchases from './lib/Purchases.svelte';
  import Learning from './lib/Learning.svelte';
  import SettingsPanel from './lib/SettingsPanel.svelte';

  // Uma seção por vez; o endereço (#radar) guarda a escolha e o botão de voltar funciona.
  const TABS = ['oportunidades', 'radar', 'compras', 'aprendizado'] as const;
  type Tab = (typeof TABS)[number];
  const fromHash = (): Tab => TABS.find((t) => t === location.hash.slice(1)) ?? 'oportunidades';
  let hash = $state<Tab>(fromHash());

  onMount(() => {
    start();
    const sync = () => { hash = fromHash(); scrollTo(0, 0); };
    addEventListener('hashchange', sync);
    return () => removeEventListener('hashchange', sync);
  });

  const count = $derived(Object.values(app.opps).filter((o) => !o.gone_at).length);
  const lostCount = $derived(lostRows().length);
  const noSaldo = $derived(app.state?.saldo_source === 'none');
  const blocked = $derived(steamBlocked());
  const byBook = $derived(bookLane());
  // Sem conta Steam não entra compra nova: a aba só existe para quem tem compra antiga.
  const hasPurchases = $derived(app.purchases.length > 0);
  const tab = $derived<Tab>(hash === 'compras' && !hasPurchases ? 'oportunidades' : hash);
  // A contagem aparece na aba do navegador: dá para ver sem voltar ao painel.
  $effect(() => { document.title = (count ? `(${count}) ` : '') + 'Skin Sniper'; });
  const tabs = $derived([
    { id: 'oportunidades', label: 'Oportunidades', count },
    { id: 'radar', label: 'Radar', count: 0 },
    ...(hasPurchases ? [{ id: 'compras', label: 'Compras', count: 0 }] : []),
    { id: 'aprendizado', label: 'Aprendizado', count: 0 }
  ]);
</script>

<TopBar {tabs} current={tab} />

{#if app.conn === 'down'}
  <div class="banner bad" role="status">Sem conexão com o Skin Sniper. Tentando de novo; a lista pode estar desatualizada.</div>
{/if}

{#if blocked && app.state}
  {@const since = app.state.hunt.last_ok_at ? ' ' + ago(app.state.hunt.last_ok_at, app.now) : ''}
  <div class="banner" role="status">
    {#if byBook}
      <b>Leitura de anúncios em pausa{since}.</b> Os preços seguem atualizando; raras e avisos esperam os anúncios.
    {:else}
      <b>A Steam recusou as consultas (429): caça em pausa{since}.</b> Reiniciar não encurta o descanso.
    {/if}
    Nova tentativa às {clock(app.state.hunt.paused_until!)}.
  </div>
{/if}

{#if app.warnings.length}
  <div class="banner" role="status">{#each app.warnings as w}<p>{w}</p>{/each}</div>
{/if}

<main>
  {#if app.loadError}
    <div class="empty" role="alert">
      <h2>Não consegui abrir o painel</h2>
      <p>{app.loadError}</p>
      <button class="btn primary" onclick={load}>Tentar de novo</button>
    </div>
  {:else if !app.loaded}
    <div class="empty" role="status"><p>Carregando…</p></div>
  {:else if tab === 'oportunidades'}
    <section aria-labelledby="h-opp">
      <h2 id="h-opp" class="sr-only">Oportunidades</h2>
      <ComboBar />
      {#if count === 0}
        <div class="empty" role="status">
          {#if noSaldo}
            <h3>Informe seu saldo</h3>
            <p>Sem ele não dá para saber quais skins cabem.</p>
            <button class="btn primary" onclick={() => (app.saldoEdit = true)}>Informar saldo</button>
          {:else}
            <h3>Nenhuma skin com lucro agora</h3>
            <p>O radar mostra quanto falta para cada uma compensar.</p>
            <a class="btn" href="#radar">Ver o radar</a>
          {/if}
        </div>
      {:else}
        <OppTable />
      {/if}
      <Watch bids />
      {#if lostCount > 0}
        <h3 class="lost">Perdidas nas últimas 24 h <span class="muted">{lostCount}</span></h3>
        <OppTable lost />
      {/if}
    </section>
  {:else if tab === 'radar'}
    <Watch />
  {:else if tab === 'compras'}
    <Purchases />
  {:else}
    <Learning />
  {/if}
</main>

<SettingsPanel />
<div class="sr-only" aria-live="polite" role="status">{app.announce}</div>

<style>
  main { padding: 16px 24px 80px; max-width: 1440px; margin: 0 auto; }
  section { display: flex; flex-direction: column; gap: 12px; }
  .banner { background: var(--amber-bg); color: var(--amber); padding: 8px var(--gutter); border-bottom: 1px solid var(--amber); }
  .banner.bad { background: var(--red-bg); color: var(--red); border-color: var(--red); }
  .banner b { color: var(--ink); }
  .lost { font-size: 14px; margin-top: 20px; }
  .lost .muted { font-weight: 400; margin-left: 4px; }
  .empty { background: var(--panel); border-radius: var(--radius); padding: 32px 24px; display: flex; flex-direction: column; gap: 8px; align-items: flex-start; }
  .empty h2, .empty h3 { font-size: 16px; }
  .empty p { color: var(--muted); max-width: 62ch; }
  .empty .btn { margin-top: 6px; }
  @media (max-width: 899px) {
    main { padding: 12px 12px 60px; }
    .banner { padding: 8px 14px; }
  }
</style>
