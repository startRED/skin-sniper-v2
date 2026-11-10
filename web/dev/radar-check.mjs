// Regressões do painel com API simulada; nunca consulta a Steam nem compra itens.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readFileSync, mkdirSync } from 'node:fs';
import { chromium } from 'playwright';

const base = 'http://127.0.0.1:5198';
const server = spawn(process.execPath, ['node_modules/vite/bin/vite.js', 'preview', '--host', '127.0.0.1', '--port', '5198', '--strictPort'], { stdio: 'ignore', windowsHide: true });
let browser;
try {
  for (let i = 0; i < 80; i++) {
    try { await fetch(base); break; } catch { await new Promise((r) => setTimeout(r, 100)); }
  }
  for (const channel of ['msedge', 'chrome', undefined]) {
    try { browser = await chromium.launch({ channel, headless: true }); break; } catch {}
  }
  assert.ok(browser, 'um navegador precisa estar instalado');
  mkdirSync('dev/shots', { recursive: true });
  for (const [tag, width, height] of [['pc', 1920, 1080], ['celular', 390, 844]]) {
    const context = await browser.newContext({ viewport: { width, height }, locale: 'pt-BR' });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', (error) => errors.push(error.message));
    await page.addInitScript(() => {
      window.alerts = [];
      window.Notification = class {
        static permission = 'granted';
        constructor(title) { window.alerts.push(title); }
      };
      window.EventSource = class extends EventTarget {
        static CLOSED = 2;
        constructor() { super(); window.stream = this; setTimeout(() => this.onopen?.(), 0); }
      };
    });
    const now = Math.floor(Date.now() / 1000);
    // Achado real arquivado, usado apenas para conferir o comportamento da tela.
    const opp = JSON.parse(readFileSync('../testdata/opportunity_stattrak_moonrise_t1_2026-11-05.json', 'utf8'));
    Object.assign(opp, { seen_at: now, checked_at: now, ev_profit_cents: null });
    const settings = { sound: false, notify_min_profit_cents: 100 };
    const key = { set: false, ok: null, detail: null };
    const state = { saldo_cents: 3606, saldo_source: 'manual', hunt: { watching: 1, queries_per_min: 6, last_query_at: now, paused_until: null, last_ok_at: now, book_ok_at: now, message: null }, combo: { ids: [], cost_cents: 0, profit_cents: 0 }, keys: { steam: key, csfloat: key, telegram: key } };
    state.hunt.message = 'Steam: seed/float em pausa · 10 min · 800 anúncios únicos · 15 novos';
    const week = { queries: 0, finds: 0, near: 0, alerts: 0, alerts_closed: 0, labeled: 0, profit_cents: 0 };
    const learning = { days: [], since: null, teacher: { ghosts: 0, waiting: 0, fast_alerts: 0, fast_alerts_closed: 0, fast_near: 0, fast_near_closed: 0, fast_ratio: null, final_alerts: 0, final_alerts_won: 0, profit_cents: 0, predicted_cents: 0, unlabeled: 0, rare_labeled: 0, rare_closed: 0 }, value: null, refit: 1, weeks: [week, week], adjustments: [] };
    await page.route('**/api/**', async (route) => {
      const path = new URL(route.request().url()).pathname;
      const body = path === '/api/state' ? state : path === '/api/opportunities' ? [opp]
        : path === '/api/settings' ? settings : path === '/api/learning' ? learning : [];
      await route.fulfill({ json: body });
    });
    const emit = (name, data) => page.evaluate(({ name, data }) => window.stream.dispatchEvent(new MessageEvent(name, { data: JSON.stringify(data) })), { name, data });
    await page.goto(base);
    await page.getByText('6 consultas totais/min', { exact: false }).waitFor();
    await page.getByText(state.hunt.message, { exact: true }).waitFor();
    await page.getByText('T1 Center Star', { exact: true }).waitFor();
    Object.assign(opp, { ev_profit_cents: 639, checked_at: now + 1 });
    await emit('opportunity', opp);
    await page.waitForFunction(() => window.alerts.length === 1);
    await emit('opportunity', { ...opp, checked_at: now + 2 });
    assert.equal(await page.evaluate(() => window.alerts.length), 1, 'reconferir não repete o aviso');
    await emit('gone', { id: opp.id, at: now, reason: 'unavailable' });
    await page.getByText('Vendido ou retirado', { exact: true }).waitFor();
    assert.equal(await page.getByRole('button', { name: 'Comprar', exact: true }).count(), 0);
    Object.assign(opp, { gone_at: now, gone_reason: 'unavailable' });
    await page.reload();
    await page.getByText('Vendido ou retirado', { exact: true }).waitFor();
    assert.equal(await page.getByText('T1 Center Star', { exact: true }).count(), 1, 'a skin fica após recarregar');
    await page.screenshot({ path: `dev/shots/radar-24h-${tag}.png`, fullPage: true });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false, 'sem rolagem horizontal');
    await page.evaluate(() => { const realNow = Date.now; Date.now = () => realNow() + 86401000; });
    await page.getByText('Vendido ou retirado', { exact: true }).waitFor({ state: 'detached' });
    assert.deepEqual(errors, [], 'nenhum erro no navegador');
    await context.close();
    console.log(`${tag}: aviso posterior único, status, recarga e retenção de 24 h conferidos`);
  }
} finally {
  await browser?.close();
  server.kill();
}
