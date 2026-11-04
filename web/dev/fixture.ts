// DADOS DE EXEMPLO — só existem no `npm run dev` (vite.config.ts não importa este arquivo no build).
// Todos os números são inventados para desenvolver a tela. Nada aqui vem da Steam.
import type { IncomingMessage, ServerResponse } from 'node:http';
import type { Plugin } from 'vite';
import type { History, Opportunity, Purchase, Sell, Settings, State, Tier } from '../src/lib/types';

export const FIXTURE_MARK = '__SKIN_SNIPER_FIXTURE__';
const nowS = () => Math.floor(Date.now() / 1000);

type Tpl = [name: string, wear: string, group: string | null, kind: 'desconto' | 'raro', price: number, midMul: number, quickMul: number, conf: Opportunity['confidence'], perDay: number];
const TPL: Tpl[] = [
  ['AK-47 | Slate', 'Field-Tested', null, 'desconto', 1850, 1.2, 0.97, 'alta', 392],
  ['M4A4 | Neo-Noir', 'Minimal Wear', null, 'desconto', 3210, 1.17, 0.99, 'alta', 128],
  ['Glock-18 | Fade', 'Factory New', 'Fade 98%', 'raro', 4390, 1.3, 1.02, 'media', 41],
  ['USP-S | Cortex', 'Field-Tested', null, 'desconto', 720, 1.28, 1.04, 'alta', 260],
  ['AWP | Atheris', 'Minimal Wear', null, 'desconto', 1290, 1.15, 0.96, 'media', 77],
  ['Desert Eagle | Printstream', 'Factory New', 'T1 Center Star', 'raro', 4780, 1.34, 1.01, 'media', 22],
  ['P250 | Sand Dune', 'Battle-Scarred', null, 'desconto', 140, 1.45, 1.1, 'alta', 510],
  ['Five-SeveN | Hyper Beast', 'Field-Tested', null, 'desconto', 2380, 1.14, 0.95, 'baixa', 9],
  ['MP9 | Starlight Protector', 'Minimal Wear', null, 'desconto', 910, 1.24, 1.03, 'alta', 190],
  ['SSG 08 | Dragonfire', 'Factory New', 'Ruby', 'raro', 3640, 1.27, 1.0, 'media', 14],
  ['Tec-9 | Fuel Injector', 'Field-Tested', null, 'desconto', 460, 1.31, 1.06, 'alta', 330],
  ['MAC-10 | Neon Rider', 'Well-Worn', null, 'desconto', 1580, 1.13, 0.93, 'baixa', 6]
];
const WEAR_FLOAT: Record<string, number> = { 'Factory New': 0.012, 'Minimal Wear': 0.09, 'Field-Tested': 0.24, 'Well-Worn': 0.41, 'Battle-Scarred': 0.62 };

function icon(name: string, n: number) {
  const hue = (n * 47) % 360;
  const tag = name.split(' ')[0].replace(/[^A-Za-z0-9]/g, '').slice(0, 3).toUpperCase();
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96"><rect width="96" height="96" fill="hsl(${hue} 22% 16%)"/><path d="M10 60 L70 30 L86 40 L26 76Z" fill="hsl(${hue} 45% 46%)"/><path d="M10 60 L26 76 L18 80 L6 66Z" fill="hsl(${hue} 30% 30%)"/><text x="48" y="90" font-family="sans-serif" font-size="12" font-weight="700" text-anchor="middle" fill="hsl(${hue} 20% 70%)">${tag}</text></svg>`;
  return `data:image/svg+xml,${encodeURIComponent(svg)}`;
}

const tier = (price: number, buy: number, etaH: number | null): Tier => {
  const net = Math.floor((price * 100) / 115);
  return { price_cents: price, net_cents: net, profit_cents: net - buy, eta_hours: etaH };
};

let seq = 0;
function genOpp(ageSec: number): Opportunity {
  const n = seq++;
  const [name, wear, group, kind, base, midMul, quickMul, conf, perDay] = TPL[n % TPL.length];
  const price = Math.round(base * (0.94 + ((n * 7) % 13) / 100));
  const mid = Math.round(price * midMul * 1.1);
  const sell: Sell = {
    quick: tier(Math.round(mid * 0.92 * quickMul), price, Math.max(1, Math.round(24 / (perDay / 5)))),
    mid: tier(mid, price, Math.max(2, Math.round(24 / (perDay / 12)))),
    high: tier(Math.round(mid * 1.12), price, Math.max(6, Math.round(24 / (perDay / 40))))
  };
  const id = `${7100000000 + n * 7919}`;
  return {
    id, name: `${name} (${wear})`, icon_url: icon(name, n),
    steam_url: `https://steamcommunity.com/market/listings/730/${encodeURIComponent(name)}`,
    inspect_url: n % 5 === 4 ? null : `steam://rungame/730/76561202255233023/+csgo_econ_action_preview%20EXEMPLO${n}`,
    wear, float: WEAR_FLOAT[wear] + ((n * 13) % 50) / 10000, seed: 100 + ((n * 211) % 900),
    group, kind, price_cents: price, sell,
    margin_pct: Math.round((sell.mid.profit_cents / price) * 1000) / 10,
    safe: sell.quick.profit_cents > 0, confidence: conf,
    evidence: `${perDay} vendas/dia nos últimos 7 dias (exemplo)`,
    seen_at: nowS() - ageSec
  };
}

let opps: Opportunity[] = [];
let purchases: Purchase[] = [];
let saldoCents = 5000;
let saldoSource: State['saldo_source'] = 'steam';
let frozen = false;
let settings: Settings = {
  steam_login_secure: '••••gLO9', csfloat_key: null, telegram_token: '••••k3Xz', telegram_chat_id: '••••6789',
  panel_url: 'http://192.168.0.10:8788', sound: true, notify_min_profit_cents: 100
};
let lastQuery = nowS();

function reset(scenario: string) {
  seq = 0;
  saldoCents = 5000;
  saldoSource = 'steam';
  opps = scenario === 'empty' ? [] : Array.from({ length: 12 }, (_, i) => genOpp(4 + i * 37 + (i % 3) * 11));
  const sold = genOpp(0);
  purchases = scenario === 'empty' ? [] : [
    { id: '7099000001', name: 'AK-47 | Redline (Field-Tested)', icon_url: icon('AK-47', 40), price_cents: 2310, status: 'bought', message: null, bought_at: nowS() - 86400, tradable_at: nowS() + 6 * 86400 - 3600, sell: sold.sell },
    { id: '7099000002', name: 'M4A1-S | Printstream (Field-Tested)', icon_url: icon('M4A1', 41), price_cents: 5090, status: 'failed', message: 'Alguém comprou antes de você.', bought_at: null, tradable_at: null, sell: sold.sell }
  ];
}
reset('full');

function combo(): State['combo'] {
  const picked: Opportunity[] = [];
  let left = saldoCents;
  for (const o of [...opps].sort((a, b) => b.sell.mid.profit_cents - a.sell.mid.profit_cents)) {
    if (o.price_cents <= left && o.safe) { picked.push(o); left -= o.price_cents; }
  }
  return { ids: picked.map((o) => o.id), cost_cents: picked.reduce((s, o) => s + o.price_cents, 0), profit_cents: picked.reduce((s, o) => s + o.sell.mid.profit_cents, 0) };
}

function state(): State {
  return {
    saldo_cents: saldoSource === 'none' ? 0 : saldoCents, saldo_source: saldoSource,
    hunt: { watching: 42, queries_per_min: 20, last_query_at: lastQuery, paused_until: null, message: 'Dados de exemplo (modo dev): nada aqui vem da Steam.' },
    combo: combo(),
    keys: {
      steam: { set: !!settings.steam_login_secure, ok: !!settings.steam_login_secure, detail: settings.steam_login_secure ? 'Conta exemplo_erick' : null },
      csfloat: { set: !!settings.csfloat_key, ok: null, detail: null },
      telegram: { set: !!settings.telegram_token, ok: settings.telegram_token ? false : null, detail: settings.telegram_token ? 'O Telegram recusou o token (exemplo de erro).' : null }
    }
  };
}

function history(o: Opportunity): History {
  let x = Number(o.id) % 9973;
  const rnd = () => ((x = (x * 16807) % 2147483647) / 2147483647);
  const base = o.sell.mid.price_cents * 0.95;
  const pts: History['points'] = [];
  let v = base;
  const t0 = nowS() - 30 * 86400;
  for (let i = 0; i < 30 * 24; i += 6) {
    v += (rnd() - 0.5) * base * 0.035 + (base - v) * 0.08;
    pts.push([t0 + i * 3600, Math.round(v), Math.round(rnd() * 9)]);
  }
  return { points: pts, sell: o.sell };
}

const clients = new Set<ServerResponse>();
const emit = (event: string, data: unknown) => {
  for (const c of clients) c.write(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
};

function send(res: ServerResponse, code: number, body: unknown) {
  res.statusCode = code;
  res.setHeader('Content-Type', 'application/json');
  res.end(JSON.stringify(body));
}

function readJson(req: IncomingMessage): Promise<any> {
  return new Promise((ok, bad) => {
    let s = '';
    req.on('data', (d) => (s += d));
    req.on('end', () => { try { ok(s ? JSON.parse(s) : {}); } catch (e) { bad(e); } });
  });
}

const SECRET_KEYS = ['steam_login_secure', 'csfloat_key', 'telegram_token', 'telegram_chat_id'] as const;

async function handle(req: IncomingMessage, res: ServerResponse) {
  const url = new URL(req.url!, 'http://x');
  const p = url.pathname;
  const m = req.method ?? 'GET';
  if (m !== 'GET' && !(req.headers['content-type'] ?? '').startsWith('application/json')) return send(res, 415, { error: 'Content-Type precisa ser application/json.' });

  if (p === '/api/events') {
    res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache', Connection: 'keep-alive' });
    res.write(': exemplo\n\n');
    clients.add(res);
    req.on('close', () => clients.delete(res));
    return;
  }
  if (p === '/api/state') return send(res, 200, state());
  if (p === '/api/opportunities') return send(res, 200, opps);
  if (p === '/api/purchases') return send(res, 200, purchases);
  if (p === '/api/settings' && m === 'GET') return send(res, 200, settings);
  if (p === '/api/settings' && m === 'PUT') {
    const patch = await readJson(req);
    for (const k of Object.keys(patch)) {
      if ((SECRET_KEYS as readonly string[]).includes(k)) (settings as any)[k] = patch[k] == null ? null : `••••${String(patch[k]).slice(-4)}`;
      else (settings as any)[k] = patch[k];
    }
    return send(res, 200, settings);
  }
  if (p === '/api/saldo' && m === 'PUT') {
    const { cents } = await readJson(req);
    if (cents == null) { saldoSource = 'steam'; saldoCents = 5000; } else { saldoSource = 'manual'; saldoCents = cents; }
    emit('state', state());
    return send(res, 200, state());
  }
  const h = /^\/api\/opportunities\/([^/]+)\/history$/.exec(p);
  if (h) {
    const o = opps.find((x) => x.id === h[1]);
    return o ? send(res, 200, history(o)) : send(res, 404, { error: 'Essa oportunidade já saiu da lista.' });
  }
  if (p === '/api/buy' && m === 'POST') {
    const { id } = await readJson(req);
    const o = opps.find((x) => x.id === id);
    if (!o) return send(res, 404, { error: 'Essa skin já foi vendida.' });
    if (o.price_cents > saldoCents) return send(res, 400, { error: 'Saldo insuficiente para essa compra.' });
    const pur: Purchase = { id: o.id, name: o.name, icon_url: o.icon_url, price_cents: o.price_cents, status: 'pending', message: null, bought_at: null, tradable_at: null, sell: o.sell };
    purchases = [pur, ...purchases.filter((x) => x.id !== o.id)];
    setTimeout(() => { Object.assign(pur, { status: 'need_confirmation' }); emit('purchase', pur); }, 1200);
    setTimeout(() => {
      Object.assign(pur, { status: 'bought', bought_at: nowS(), tradable_at: nowS() + 7 * 86400 });
      saldoCents -= o.price_cents;
      opps = opps.filter((x) => x.id !== o.id);
      emit('purchase', pur); emit('gone', { id: o.id }); emit('state', state());
    }, 5500);
    return send(res, 200, pur);
  }
  if (p === '/api/__dev/scenario' && m === 'POST') {
    const b = await readJson(req);
    reset(b.name ?? 'full');
    frozen = !!b.freeze;
    return send(res, 200, { ok: true });
  }
  send(res, 404, { error: 'Rota não existe.' });
}

export function fixture(): Plugin {
  return {
    name: 'skin-sniper-fixture',
    apply: 'serve',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        if (!req.url?.startsWith('/api/')) return next();
        handle(req, res).catch((e) => send(res, 500, { error: String(e) }));
      });
      let tick = 0;
      setInterval(() => {
        if (frozen) return;
        tick++;
        lastQuery = nowS() - 1;
        emit('state', state());
        if (tick % 2 === 0) {
          if (opps.length > 9 && tick % 6 === 0) {
            const gone = opps[Math.floor(Math.random() * opps.length)];
            opps = opps.filter((o) => o !== gone);
            emit('gone', { id: gone.id });
          } else {
            const o = genOpp(0);
            opps.push(o);
            emit('opportunity', o);
          }
        }
      }, 5000);
      setInterval(() => { for (const c of clients) c.write(': ping\n\n'); }, 15000);
    }
  };
}
