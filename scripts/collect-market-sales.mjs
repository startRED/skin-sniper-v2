#!/usr/bin/env node
import assert from 'node:assert/strict';
import { appendFile, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';

const WEAR_PREFERENCE = ['Field-Tested', 'Minimal Wear', 'Factory New', 'Battle-Scarred', 'Well-Worn'];
const SPECIAL_PATTERNS = /moonrise|heat[- ]?treated|case hardened|fade|sandstorm|kami|\bpaw\b|seasons/i;
const MIN_REQUEST_GAP_MS = 500;
const REQUEST_TIMEOUT_MS = 15_000;
const BACKOFF_429_MS = 60_000;
const BASE_URL = 'https://market.csgo.com/api/v2/full-history';

function buildTargets(skins, marketIndex) {
  assert(Array.isArray(skins), 'skins.json precisa ser uma lista');
  assert(marketIndex?.history && typeof marketIndex.history === 'object', 'market_index.json precisa ter history');
  const targetByName = new Map();
  const wearAvailable = (skin, wear) => skin.wears?.some(row => row.name === wear)
    && Object.hasOwn(marketIndex.history, `${skin.name} (${wear})`);
  for (const skin of skins) {
    const weaponId = Number(skin.weapon?.weapon_id);
    if (!skin.name || skin.name.startsWith('★') || !Number.isInteger(weaponId) || weaponId >= 100) continue;
    const wear = WEAR_PREFERENCE.find(candidate => wearAvailable(skin, candidate));
    if (wear) targetByName.set(`${skin.name} (${wear})`, { name: `${skin.name} (${wear})`, base: skin.name });
  }
  for (const skin of skins) {
    const weaponId = Number(skin.weapon?.weapon_id);
    if (!skin.name || skin.name.startsWith('★') || !Number.isInteger(weaponId) || weaponId >= 100 || !SPECIAL_PATTERNS.test(skin.name)) continue;
    for (const wear of WEAR_PREFERENCE) {
      if (wearAvailable(skin, wear)) targetByName.set(`${skin.name} (${wear})`, { name: `${skin.name} (${wear})`, base: skin.name });
    }
  }
  return [...targetByName.values()];
}

function parseSalesPayload(payload, nowSeconds = Math.floor(Date.now() / 1000)) {
  const data = payload?.data;
  const time = payload?.time;
  assert(Number.isInteger(time) && time > 0, 'time ausente ou inválido');
  assert(time <= nowSeconds + 120, 'time do servidor está no futuro');
  assert(Number.isInteger(data?.id) && data.id > 0, 'data.id ausente ou inválido');
  for (const key of ['sales7d', 'sales30d']) {
    const count = data[key]?.USD;
    assert(Number.isInteger(count) && count >= 0, `${key}.USD ausente ou inválido`);
  }
  return {
    time,
    id: data.id,
    sales7d: data.sales7d.USD,
    sales30d: data.sales30d.USD,
    source_time_valid: true,
    counts_valid: true,
  };
}

function parseFixturePayload(text) {
  return JSON.parse(text);
}

async function selfTest() {
  const fixture = parseFixturePayload(await readFile(new URL('../testdata/market_sales_sample.json', import.meta.url), 'utf8'));
  const row = parseSalesPayload(fixture, fixture.time);
  assert.deepEqual(Object.keys(row), ['time', 'id', 'sales7d', 'sales30d', 'source_time_valid', 'counts_valid']);
  assert.throws(() => parseSalesPayload({ ...fixture, data: { ...fixture.data, sales7d: { USD: 1.5 } } }, fixture.time), /sales7d/);
  assert.throws(() => parseSalesPayload({ ...fixture, time: fixture.time + 121 }, fixture.time), /futuro/);
  const index = { history: { 'AK-47 | Demo (Field-Tested)': 1, 'AK-47 | Demo (Minimal Wear)': 2, 'AK-47 | Moonrise (Battle-Scarred)': 3 } };
  const skins = [
    { name: 'AK-47 | Demo', weapon: { weapon_id: 7 }, wears: [{ name: 'Minimal Wear' }, { name: 'Field-Tested' }] },
    { name: 'AK-47 | Moonrise', weapon: { weapon_id: 7 }, wears: [{ name: 'Battle-Scarred' }] },
    { name: '★ Demo Knife', weapon: { weapon_id: 7 }, wears: [{ name: 'Field-Tested' }] },
  ];
  assert.deepEqual(buildTargets(skins, index), [
    { name: 'AK-47 | Demo (Field-Tested)', base: 'AK-47 | Demo' },
    { name: 'AK-47 | Moonrise (Battle-Scarred)', base: 'AK-47 | Moonrise' },
  ]);
  console.log('Self-test passou.');
}

async function readSuccessfulNames(outputPath) {
  try {
    const text = await readFile(outputPath, 'utf8');
    return new Set(text.split(/\r?\n/).filter(Boolean).map(line => JSON.parse(line).name));
  } catch (error) {
    if (error.code === 'ENOENT') return new Set();
    throw error;
  }
}

async function recordFailure(failurePath, target, status, message) {
  await appendFile(failurePath, `${JSON.stringify({ name: target.name, base: target.base, status, error: message })}\n`, 'utf8');
}

async function collect(researchDir) {
  const skinsPath = path.join(researchDir, 'skins.json');
  const indexPath = path.join(researchDir, 'market_index.json');
  const outputPath = path.join(researchDir, 'market_volumes.jsonl');
  const failurePath = path.join(researchDir, 'market_volumes_failures.jsonl');
  const [skins, marketIndex] = await Promise.all([
    readFile(skinsPath, 'utf8').then(JSON.parse),
    readFile(indexPath, 'utf8').then(JSON.parse),
  ]);
  const targets = buildTargets(skins, marketIndex);
  const alreadyDone = await readSuccessfulNames(outputPath);
  const pending = targets.filter(target => !alreadyDone.has(target.name));
  let lastStart = 0;
  let successes = 0;
  let failures = 0;
  let retryable = [];

  console.log(`Alvos ${targets.length}; já registrados ${alreadyDone.size}; nesta execução ${pending.length}.`);
  const requestOne = async (target, backoffs429 = 0) => {
    const wait = MIN_REQUEST_GAP_MS - (Date.now() - lastStart);
    if (lastStart && wait > 0) await delay(wait);
    lastStart = Date.now();
    let response;
    try {
      response = await fetch(`${BASE_URL}/${marketIndex.history[target.name]}.json`, { signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS) });
    } catch (error) {
      return { kind: 'failure', status: 'network', message: error.name || 'fetch failed' };
    }
    if (response.status === 429) {
      await response.body?.cancel();
      if (backoffs429 >= 3) return { kind: 'failure', status: 429, message: 'rate limit persisted after three 60-second backoffs' };
      console.log(`429; aguardando ${BACKOFF_429_MS / 1000}s antes de repetir ${target.name}.`);
      await delay(BACKOFF_429_MS);
      return requestOne(target, backoffs429 + 1);
    }
    if (response.status >= 500) {
      await response.body?.cancel();
      return { kind: 'retry', status: response.status, message: 'server error' };
    }
    if (!response.ok) {
      await response.body?.cancel();
      return { kind: 'failure', status: response.status, message: 'HTTP error' };
    }
    try {
      const payload = await response.json();
      const parsed = parseSalesPayload(payload);
      if (parsed.id !== marketIndex.history[target.name]) throw new Error('data.id difere do ID do índice');
      return { kind: 'success', row: { name: target.name, base: target.base, ...parsed } };
    } catch (error) {
      return { kind: 'failure', status: 'invalid_payload', message: error.message };
    }
  };

  const saveResult = async (target, result, mayRetry = true) => {
    if (result.kind === 'success') {
      await appendFile(outputPath, `${JSON.stringify(result.row)}\n`, 'utf8');
      successes++;
      if (successes % 200 === 0) console.log(`Progresso: ${alreadyDone.size + successes}/${targets.length}.`);
    } else if (result.kind === 'retry' && mayRetry) retryable.push({ target, result });
    else {
      await recordFailure(failurePath, target, result.status, result.message);
      failures++;
    }
  };

  for (const target of pending) await saveResult(target, await requestOne(target));
  for (const { target } of retryable) await saveResult(target, await requestOne(target), false);
  console.log(`Concluído nesta execução: ${successes} respostas válidas; falhas finais registradas: ${failures}.`);
}

const args = process.argv.slice(2);
if (args.length === 1 && args[0] === '--self-test') await selfTest();
else if (args.length === 1 && !args[0].startsWith('-')) await collect(path.resolve(args[0]));
else {
  console.error('Uso: node scripts/collect-market-sales.mjs <pasta-research> | --self-test');
  process.exitCode = 2;
}
