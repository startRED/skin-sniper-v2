// Prova da lista opcional com dados de exemplo e navegador sem janela.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const base = 'http://127.0.0.1:5209';
const output = new URL('../../.faber/verify/bets/', import.meta.url);
mkdirSync(output, { recursive: true });
const vite = spawn(process.execPath, ['node_modules/vite/bin/vite.js', '--host', '127.0.0.1', '--port', '5209', '--strictPort'], { cwd: new URL('../', import.meta.url), windowsHide: true });
let browser;
try {
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error('O painel de exemplo não iniciou.')), 30000);
    vite.stdout.on('data', chunk => {
      if (chunk.toString().includes('Local:')) { clearTimeout(timeout); resolve(); }
    });
    vite.once('exit', code => { clearTimeout(timeout); reject(new Error(`Servidor saiu: ${code}`)); });
    vite.once('error', reject);
  });
  for (const channel of ['msedge', 'chrome', undefined]) {
    try { browser = await chromium.launch({ channel, headless: true }); break; } catch {}
  }
  assert.ok(browser, 'Nenhum navegador disponível.');
  for (const [name, width, height] of [['desktop', 1440, 900], ['celular', 390, 844]]) {
    const page = await browser.newPage({ viewport: { width, height }, locale: 'pt-BR' });
    await page.clock.install();
    const now = Math.floor(Date.now() / 1000);
    const tier = (price, net) => ({ price_cents: price, net_cents: net, profit_cents: net - 2650, eta_hours: null });
    let requests = 0;
    await page.route('**/api/bets', async route => {
      requests++;
      await route.fulfill({ json: requests > 1 ? [] : [{
        id: 'aposta', name: 'Glock-18 | Moonrise (Minimal Wear)', group: 'T1 Center Star', seed: 837,
        price_cents: 2650, checked_at: now - 900, seen_at: now - 900, sell: { quick: tier(889, 773), mid: tier(3408, 2964), high: tier(3800, 3305) },
        evidence: '6 anúncios parecidos sumiram na Steam · venda não confirmada',
        steam_url: 'https://steamcommunity.com/market/listings/730/Glock-18%20%7C%20Moonrise%20%28Minimal%20Wear%29'
      }] });
    });
    await page.goto(base);
    const toggle = page.getByRole('button', { name: 'Ver apostas mais arriscadas' });
    await toggle.waitFor({ timeout: 5000 });
    assert.equal(requests, 0);
    await toggle.focus();
    await toggle.press('Enter');
    const panel = page.getByRole('region', { name: 'Apostas mais arriscadas' });
    await panel.getByText('Glock-18 | Moonrise (Minimal Wear)', { exact: true }).waitFor();
    assert.match(await panel.innerText(), /18,77/);
    assert.match(await panel.innerText(), /3,14/);
    assert.match(await panel.innerText(), /venda não confirmada/);
    assert.match(await panel.innerText(), /sem reconferir há 15 min/);
    assert.match(await panel.getByRole('link', { name: 'Abrir na Steam' }).getAttribute('href'), /^https:\/\/steamcommunity\.com\//);
    await panel.scrollIntoViewIfNeeded();
    await page.screenshot({ path: fileURLToPath(new URL(`${name}.png`, output)) });
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
    await page.clock.fastForward(15_000);
    await panel.getByText('Nenhuma aposta encontrada nas últimas 3 horas.').waitFor();
    assert.equal(requests, 2);
    await panel.getByRole('button', { name: 'Atualizar apostas' }).click();
    await panel.getByText('Nenhuma aposta encontrada nas últimas 3 horas.').waitFor();
    await page.getByRole('button', { name: 'Ocultar apostas mais arriscadas' }).click();
    assert.equal(await panel.count(), 0);
    assert.equal(requests, 3);
    await page.close();
  }
  console.log('Apostas: abertura por teclado, preço, perda, evidência, atualização e bordas conferidos no desktop e no celular.');
} finally {
  await browser?.close();
  vite.kill();
}
