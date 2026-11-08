// Referências públicas só fazem triagem; giro e preço final continuam medidos pela caça.
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const read = p => JSON.parse(fs.readFileSync(p, 'utf8'));
const bands = [100, 1000, 10000, 50000, Infinity]; // USD; não converte em saldo Steam.
const wearBounds = {
  'Factory New': [0, .07], 'Minimal Wear': [.07, .15], 'Field-Tested': [.15, .38],
  'Well-Worn': [.38, .45], 'Battle-Scarred': [.45, 1],
};

export function cents(value) {
  if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) return null;
  const match = String(value).match(/^(\d+)(?:\.(\d{1,2}))?$/);
  if (!match) return null;
  const result = Number(match[1]) * 100 + Number((match[2] ?? '').padEnd(2, '0'));
  return Number.isSafeInteger(result) && result > 0 ? result : null;
}

export function codeFor(def, paint) {
  assert(Number.isInteger(def) && def > 0 && def < 100);
  assert(Number.isInteger(paint) && paint > 0 && paint < 100000);
  const varint = n => { const bytes = []; do { const low = n & 127; n >>>= 7; bytes.push(low | (n ? 128 : 0)); } while (n); return bytes; };
  return 'G' + Buffer.from([24, ...varint(def), 32, ...varint(paint), 48, 4]).toString('hex').toUpperCase();
}

export function build(skins, prices, { at, generatedAt = at, patterns = new Set(), metrics = new Map(), steamMetrics = new Map() }) {
  assert(Array.isArray(skins) && skins.length > 0 && prices && !Array.isArray(prices));
  assert(Number.isSafeInteger(at) && at > 0);
  assert(Number.isSafeInteger(generatedAt) && generatedAt >= at);
  const universe = new Map(), floats = {}, rejected = {}, groups = new Map();
  const reject = why => { rejected[why] = (rejected[why] ?? 0) + 1; };
  for (const s of skins) {
    if (typeof s.name !== 'string' || s.name.startsWith('★') || !s.name.includes(' | ')
      || !Number.isInteger(s.weapon?.weapon_id) || s.weapon.weapon_id >= 100) continue;
    const paint = Number(s.paint_index), def = s.weapon.weapon_id;
    assert(Number.isInteger(paint) && paint > 0 && paint < 100000, 'paint_index inválido');
    assert(Number.isFinite(s.min_float) && Number.isFinite(s.max_float)
      && s.min_float >= 0 && s.min_float < s.max_float && s.max_float <= 1, 'float inválido');
    floats[s.name] = [s.min_float, s.max_float];
    for (const wear of s.wears ?? []) {
      const bounds = wearBounds[wear.name];
      assert(bounds && Math.max(bounds[0], s.min_float) < Math.min(bounds[1], s.max_float), 'desgaste impossível');
      for (const prefix of ['', ...(s.stattrak ? ['StatTrak™ '] : []), ...(s.souvenir ? ['Souvenir '] : [])]) {
        const name = `${prefix}${s.name} (${wear.name})`;
        const prev = universe.get(name);
        // Acabamentos/fases com o mesmo nome precisam de resolução própria da Steam.
        if (prev) { if (prev.paint_index !== paint || prev.def !== def) prev.ambiguous = true; continue; }
        universe.set(name, { name, base: s.name, def, paint_index: paint, wear: wear.name,
          variant: prefix ? prefix.trim() : 'normal', icon_url: s.image ?? '' });
      }
    }
  }
  const candidates = [];
  for (const row of universe.values()) {
    if (row.ambiguous) { reject('ambiguous_code'); continue; }
    const p = prices[row.name] ?? {};
    const daily = cents(p.last_24h), week = cents(p.last_7d), month = cents(p.last_30d);
    const special = patterns.has(row.base) || row.base === 'AWP | Asiimov';
    const reference = daily ?? week ?? (special ? month : null);
    if (!reference) { reject('no_recent_reference'); continue; }
    const stable = daily && week && daily * 2 >= week && daily <= week * 2
      && (!month || (week * 2 >= month && week <= month * 2));
    const lowFloat = row.variant === 'normal' && ['Factory New', 'Minimal Wear'].includes(row.wear) && reference >= 10;
    if (!special && ((!stable) || (reference < 100 && !lowFloat))) { reject('stale_unstable_or_too_cheap'); continue; }
    const metric = metrics.get(row.name) ?? metrics.get(row.base);
    // Ausência de vendas em outro mercado não prova ausência na Steam: mantém a candidata, com prioridade menor.
    const sales30 = metric?.sales30d ?? null;
    const steam = steamMetrics.get(row.name);
    const steamAge = generatedAt - steam?.history_at;
    const steamLiquid = steam?.sales_per_day >= 1 && steamAge >= 0 && steamAge <= 7 * 86400;
    // Corte conservador de pesquisa, não prova de falta de demanda Steam; os raros têm proteção própria.
    if (!special && sales30 !== null && sales30 < 7 && !steamLiquid) { reject('low_external_turnover'); continue; }
    const score = (special ? 1000 : 0) + (sales30 === null ? 0 : Math.log2(1 + sales30) * 10)
      + (row.variant === 'normal' ? 5 : 0) + (lowFloat ? 2 : 0);
    const item = { name: row.name, code: codeFor(row.def, row.paint_index), paint_index: row.paint_index,
      reference_usd_cents: reference, rank: 0, icon_url: row.icon_url,
      base: row.base, wear: row.wear, variant: row.variant, reasons: [special ? 'pattern_or_high_float' : lowFloat && reference < 100 ? 'low_float_candidate' : 'recent_stable_reference'],
      market_sales30d: sales30, market_sample_name: metric?.name ?? null, score };
    const band = bands.findIndex(max => reference < max);
    const key = row.base.split(' | ')[0] + ':' + band;
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(item);
    candidates.push(item);
  }
  // Intercala arma e faixa de preço: o catálogo grande não vira uma fila só de AKs caras.
  for (const rows of groups.values()) rows.sort((a, b) => b.score - a.score || a.name.localeCompare(b.name, 'en'));
  const items = [], buckets = [...groups].sort(([a], [b]) => a.localeCompare(b, 'en')).map(([, v]) => v);
  for (let i = 0; ; i++) {
    let added = 0;
    for (const bucket of buckets) if (bucket[i]) { const { score, ...item } = bucket[i]; item.rank = items.length + 200000; items.push(item); added++; }
    if (!added) break;
  }
  const report = { universe_versions: universe.size, universe_skins: Object.keys(floats).length, curated_versions: items.length,
    curated_skins: new Set(items.map(i => i.base)).size, weapons: new Set(items.map(i => i.base.split(' | ')[0])).size, rejected,
    price_bands_usd: Object.fromEntries(bands.map((max, i) => [`${i ? bands[i - 1] / 100 : 0}-${Number.isFinite(max) ? max / 100 : '+'}`, items.filter(x => bands.findIndex(b => x.reference_usd_cents < b) === i).length])),
    variants: Object.fromEntries(['normal', 'StatTrak™', 'Souvenir'].map(v => [v, items.filter(i => i.variant === v).length])),
    with_market_base_sample: items.filter(i => i.market_sales30d !== null).length };
  return { version: 1, generated_at: generatedAt, reference_observed_at: at, sources: {
    metadata: 'https://raw.githubusercontent.com/ByMykel/CSGO-API/main/public/api/en/skins.json',
    reference: 'https://prices.csgotrader.app/latest/steam.json',
    liquidity: 'https://market.csgo.com/en/api/content/start',
    limitations: 'Médias públicas USD são triagem, não piso. Giro Market.CSGO é da variante amostrada, não de toda família, nem da Steam. Preço final e prêmio exigem leituras da caça.' },
    report, items, floats };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.argv.includes('--self-test')) {
    assert.equal(cents(1.01), 101); assert.equal(cents(.29), 29); assert.equal(cents(-1), null); assert.equal(cents(NaN), null);
    assert.equal(codeFor(4, 694), 'G180420B6053004'); assert.equal(codeFor(64, 800), 'G184020A0063004');
    const fixture = read(path.join(root, 'testdata/catalog_sources_sample.json'));
    const result = build(fixture.skins, fixture.prices, { at: 1794099600, patterns: new Set(['Glock-18 | Moonrise']) });
    assert(result.items.some(i => i.name === 'Glock-18 | Moonrise (Field-Tested)'));
    assert(result.items.every(i => !i.name.startsWith('★') && i.reference_usd_cents > 0));
    assert(result.items.every(i => i.sales_per_day === undefined && i.floor_usd_cents === undefined));
    const base = 'Glock-18 | Moonrise';
    const quiet = build(fixture.skins, fixture.prices, { at: 1794099600, metrics: new Map([[base, { sales30d: 0 }]]) });
    assert(!quiet.items.some(i => i.base === base), 'giro externo baixo sem exceção medido deve sair');
    const protectedSkin = base + ' (Field-Tested)';
    const measured = build(fixture.skins, fixture.prices, { at: 1794099600, metrics: new Map([[base, { sales30d: 0 }]]),
      steamMetrics: new Map([[protectedSkin, { sales_per_day: 10, history_at: 1794099600 }]]) });
    assert(measured.items.some(i => i.name === protectedSkin), 'demanda Steam medida protege a candidata');
    const duplicate = structuredClone(fixture.skins.find(s => s.name === base)); duplicate.paint_index = String(Number(duplicate.paint_index) + 1);
    const ambiguous = build([...fixture.skins, duplicate], fixture.prices, { at: 1794099600 });
    assert(!ambiguous.items.some(i => i.base === base), 'código ambíguo não pode ser inventado');
    console.log('catalog source checks passed');
  } else {
    const dir = path.resolve(process.argv[2] ?? path.join(root, 'research'));
    const metrics = new Map();
    const file = path.join(dir, 'market_volumes.jsonl');
    if (fs.existsSync(file)) for (const line of fs.readFileSync(file, 'utf8').trim().split('\n').filter(Boolean)) {
      const row = JSON.parse(line);
      const age = Math.floor(Date.now() / 1000) - row.time;
      if (!row.error && age >= 0 && age <= 2 * 86400 && Number.isSafeInteger(row.sales30d) && row.sales30d >= 0) {
        metrics.set(row.name, row); if (!metrics.has(row.base)) metrics.set(row.base, row);
      }
    }
    const patterns = new Set(Object.keys(read(path.join(root, 'assets/patterns/cspatterns.json')).skins));
    for (const base of Object.keys(read(path.join(root, 'assets/patterns/researched.json')).skins)) patterns.add(base);
    for (const w of ['AK-47', 'Five-SeveN', 'MAC-10']) patterns.add(w + ' | Case Hardened');
    const skins = read(path.join(dir, 'skins.json')), prices = read(path.join(dir, 'steam_prices.json'));
    const at = read(path.join(dir, 'sources.json')).reference_observed_at;
    const steamFile = path.join(dir, 'steam_metrics.json');
    const steamMetrics = new Map(fs.existsSync(steamFile) ? read(steamFile).map(i => [i.name, i]) : []);
    const result = build(skins, prices, { at, generatedAt: Math.floor(Date.now() / 1000), patterns, metrics, steamMetrics });
    const { floats, ...catalog } = result;
    fs.mkdirSync(path.join(root, 'assets/catalog'), { recursive: true });
    fs.writeFileSync(path.join(root, 'assets/catalog/curated.json'), JSON.stringify(catalog) + '\n');
    fs.writeFileSync(path.join(root, 'assets/patterns/floats.json'), JSON.stringify({ _source: catalog.sources.metadata, skins: floats }) + '\n');
    console.log(JSON.stringify(catalog.report, null, 2));
  }
}
