// Capturas de tela em navegador sem janela, contra o servidor de exemplo do `npm run dev`.
// Uso: npm run shots   (saída em dev/shots/)
import { spawn } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const PORT = 5199;
const BASE = `http://127.0.0.1:${PORT}`;
const out = fileURLToPath(new URL('./shots/', import.meta.url));
mkdirSync(out, { recursive: true });

const vite = spawn(process.execPath, ['node_modules/vite/bin/vite.js', '--port', String(PORT), '--host', '127.0.0.1', '--strictPort'], { stdio: 'ignore' });
for (let i = 0; i < 60; i++) {
  try { await fetch(BASE); break; } catch { await new Promise((r) => setTimeout(r, 250)); }
}

let browser;
for (const channel of ['msedge', 'chrome', undefined]) {
  try { browser = await chromium.launch({ channel, headless: true }); break; } catch {}
}
if (!browser) throw new Error('Nenhum navegador encontrado para as capturas.');

const scenario = (name, freeze = true) =>
  fetch(`${BASE}/api/__dev/scenario`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ name, freeze }) });

try {
  for (const [tag, width, height] of [['pc', 1920, 1080], ['celular', 390, 844]]) {
    const ctx = await browser.newContext({ viewport: { width, height }, deviceScaleFactor: tag === 'pc' ? 1 : 2, locale: 'pt-BR' });
    const page = await ctx.newPage();
    const shot = (name, opts = {}) => page.screenshot({ path: `${out}${tag}-${name}.png`, ...opts });

    await scenario('full');
    await page.goto(BASE);
    await page.waitForSelector('.item');
    await page.waitForTimeout(600);
    await shot('1-lista-cheia');

    await page.locator('.skinbtn').nth(1).click();
    await page.waitForSelector('.detail svg');
    await page.locator('.item.open').scrollIntoViewIfNeeded();
    await page.locator('.item.open .cf').hover();
    await page.waitForTimeout(300);
    await shot('2-linha-expandida');
    await page.locator('.skinbtn').nth(1).click();

    await page.evaluate(() => window.scrollTo(0, 0));
    await page.locator('.buy .btn.primary').first().click();
    await page.waitForTimeout(300);
    await shot('3-confirmar-compra');
    // segundo clique: pede confirmação no app Steam
    await page.locator('.buy .btn.primary').first().click();
    await page.waitForSelector('text=Confirme no app Steam', { timeout: 6000 });
    await shot('4-aguardando-steam');

    await page.locator('#h-compras').scrollIntoViewIfNeeded();
    await shot('5-compras');
    await page.evaluate(() => window.scrollTo(0, 0));

    await page.getByRole('button', { name: 'Abrir configurações' }).click();
    await page.waitForTimeout(300);
    await shot('6-configuracoes');
    await page.keyboard.press('Escape');

    await scenario('empty');
    await page.reload();
    await page.waitForSelector('.empty');
    await page.waitForTimeout(300);
    await shot('7-vazio');
    await ctx.close();
  }
} finally {
  await browser.close();
  vite.kill();
}
