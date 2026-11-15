// Prova do Aprendizado com dados de exemplo e navegador sem janela.
import { spawn } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { chromium } from 'playwright';
import assert from 'node:assert/strict';

const port = 5207;
const base = `http://127.0.0.1:${port}`;
const output = new URL('../../.faber/verify/learning/', import.meta.url);
mkdirSync(output, { recursive: true });
const vite = spawn(process.execPath, ['node_modules/vite/bin/vite.js', '--host', '127.0.0.1', '--port', String(port), '--strictPort'], { cwd: new URL('../', import.meta.url), stdio: ['ignore', 'pipe', 'pipe'] });
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
  for (const [name, width, height] of [['desktop', 1440, 1000], ['celular', 390, 844]]) {
    const page = await browser.newPage({ viewport: { width, height }, locale: 'pt-BR' });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto(`${base}/#aprendizado`);
    await page.getByRole('button', { name: 'Testar aviso', exact: true }).waitFor();
    await page.screenshot({ path: new URL(`${name}.png`, output).pathname.replace(/^\/(\w:)/, '$1'), fullPage: true });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false, 'A página passou da borda.');
    await page.screenshot({ path: new URL(`${name}.png`, output).pathname.replace(/^\/(\w:)/, '$1'), fullPage: true });
    assert.equal(await page.locator('.metrics > li').count(), 7);
    const aboveSaldo = page.locator('.alerts > li').filter({ hasText: 'acima do saldo' });
    assert.equal(await aboveSaldo.count(), 1);
    assert.equal(await aboveSaldo.getByText('Windows', { exact: true }).count(), 0);
    assert.match(await aboveSaldo.innerText(), /falhou: Telegram recusou/);
    await aboveSaldo.screenshot({ path: new URL(`${name}-acima-do-saldo.png`, output).pathname.replace(/^\/(\w:)/, '$1') });
    const dailyAlerts = page.locator('.metrics > li').nth(3);
    assert.match(await dailyAlerts.innerText(), /meta 12 dentro do saldo/);
    assert.match(await dailyAlerts.innerText(), /3 em 7 dias; 2 dentro do saldo/);
    await dailyAlerts.screenshot({ path: new URL(`${name}-meta-avisos.png`, output).pathname.replace(/^\/(\w:)/, '$1') });
    const comparison = page.locator('.metrics > li').first();
    assert.match(await comparison.innerText(), /não comparáveis/);
    assert.match(await comparison.innerText(), /alvos e intervalos diferentes/);
    assert.match(await comparison.innerText(), /não guarda a faixa lida/);
    assert.equal(await comparison.locator('.muted').innerText(), 'inconclusivo');
    await comparison.screenshot({ path: new URL(`${name}-comparacao.png`, output).pathname.replace(/^\/(\w:)/, '$1') });
    await page.evaluate(() => scrollTo(0, 0));
    await page.getByRole('button', { name: 'Testar aviso', exact: true }).click();
    await page.getByRole('button', { name: 'apareceu', exact: true }).click();
    await page.getByText(/Resposta salva em .*: apareceu/).waitFor();
    await page.reload();
    await page.getByText(/Resposta salva em .*: apareceu/).waitFor();
    await page.getByRole('button', { name: 'Testar aviso', exact: true }).click();
    await page.getByRole('button', { name: 'não apareceu', exact: true }).click();
    await page.getByText(/Resposta salva em .*: não apareceu/).waitFor();
    await page.reload();
    await page.getByText(/Resposta salva em .*: não apareceu/).waitFor();
    await page.screenshot({ path: new URL(`${name}-teste.png`, output).pathname.replace(/^\/(\w:)/, '$1') });
    await page.route('**/api/learning/test', route => route.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify({ error: 'Falha de exemplo ao enviar.' }) }));
    await page.getByRole('button', { name: 'Testar aviso', exact: true }).click();
    await page.getByRole('alert').filter({ hasText: 'Falha de exemplo ao enviar.' }).waitFor();
    assert.equal(await page.getByRole('button', { name: 'Testar aviso', exact: true }).isEnabled(), true);
    assert.deepEqual(errors, [], 'Erros no navegador.');
    console.log(`${name}: tela carregada e sem ultrapassar a largura`);
    await page.close();
  }
} finally {
  await browser?.close();
  vite.kill();
}
