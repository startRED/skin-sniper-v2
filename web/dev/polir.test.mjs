// Testes de interface com dados fictícios; não acessam a Steam.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { chromium } from 'playwright';

const base = 'http://127.0.0.1:5201';
const server = spawn(process.execPath, ['node_modules/vite/bin/vite.js', '--port', '5201', '--host', '127.0.0.1', '--strictPort'], { stdio: 'ignore' });
let browser;
try {
  for (let i = 0; i < 80; i++) {
    try { if ((await fetch(base)).ok) break; } catch {}
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  await fetch(`${base}/api/__dev/scenario`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ name: 'full', freeze: true }) });
  const state = await (await fetch(`${base}/api/state`)).json();
  const examples = await (await fetch(`${base}/api/opportunities`)).json();
  const make = (index, name, daily, profit) => ({ ...examples[index], name, wear: null, profit_per_day_cents: daily, sell: { ...examples[index].sell, mid: { ...examples[index].sell.mid, profit_cents: profit } } });
  const opportunities = [make(0, 'Positiva', 30, 50), make(1, 'Rara sem prazo', null, 70), make(2, 'Negativa', -10, 60)];
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  await page.route('**/api/state', (route) => route.fulfill({ json: state }));
  await page.route('**/api/opportunities', (route) => route.fulfill({ json: opportunities }));
  await page.goto(base);
  await page.locator('.item').first().waitFor();
  const names = () => page.locator('.item .nm').allTextContents();
  assert.deepEqual(await names(), ['Positiva', 'Rara sem prazo', 'Negativa']);
  await page.getByRole('button', { name: 'Por dia' }).click();
  assert.deepEqual(await names(), ['Negativa', 'Rara sem prazo', 'Positiva']);
  assert.equal(await page.getByText('Sem dados', { exact: true }).count(), 1);

  state.keys.steam = { set: true, ok: false, detail: 'o login funcionou, mas a Steam não liberou o Mercado para esta conta' };
  await page.reload();
  await page.getByRole('button', { name: 'Abrir configurações' }).click();
  assert.equal(await page.getByText('O login funcionou, mas a Steam não liberou o Mercado', { exact: false }).count(), 1);
  assert.equal(await page.getByText('Login expirado', { exact: false }).count(), 0);

  state.keys.steam = { set: true, ok: true, detail: 'conta conectada' };
  await page.reload();
  await page.getByRole('button', { name: 'Abrir configurações' }).click();
  await page.route('**/api/steam/login/qr', (route) => route.fulfill({ status: 500, json: { error: 'Falha ao gerar QR de teste.' } }));
  await page.getByRole('button', { name: 'Entrar com o app da Steam' }).click();
  await page.getByRole('alert').filter({ hasText: 'Falha ao gerar QR de teste.' }).waitFor();
  await page.unroute('**/api/steam/login/qr');
  await page.route('**/api/steam/login/status', (route) => route.fulfill({ json: { state: 'expired' } }));
  await page.getByRole('button', { name: 'Entrar com o app da Steam' }).click();
  await page.getByText('O QR venceu.', { exact: false }).waitFor({ timeout: 7000 });

  await fetch(`${base}/api/__dev/scenario`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ name: 'empty', freeze: true }) });
  await page.unroute('**/api/opportunities');
  await page.reload();
  await page.getByText('Nenhuma skin com lucro agora.', { exact: false }).waitFor();
  assert.equal(await page.getByText('20 anúncios conferidos', { exact: false }).count(), 1);
  assert.ok(await page.getByText('testando alvo novo', { exact: true }).count() > 0);
  assert.ok(await page.getByText('Float', { exact: true }).count() > 0);
  assert.ok(await page.getByText('Seed', { exact: true }).count() > 0);
  console.log('Ordenação, Mercado, erros de QR e lista de vigiados: OK');
} finally {
  await browser?.close();
  server.kill();
}
