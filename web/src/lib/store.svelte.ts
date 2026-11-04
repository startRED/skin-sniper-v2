import { api, ApiError } from './api';
import { money, signed } from './format';
import { beep, unlockAudio } from './sound';
import type { History, Opportunity, Purchase, Settings, SettingsInput, State } from './types';

export type SortKey = 'profit' | 'margin' | 'safe' | 'eta' | 'new';
const DEFAULT_DIR: Record<SortKey, 'asc' | 'desc'> = { profit: 'desc', margin: 'desc', safe: 'desc', eta: 'asc', new: 'desc' };

export const app = $state({
  loaded: false,
  loadError: null as string | null,
  conn: 'connecting' as 'connecting' | 'live' | 'down',
  state: null as State | null,
  opps: {} as Record<string, Opportunity>,
  settings: null as Settings | null,
  purchases: [] as Purchase[],
  now: Math.floor(Date.now() / 1000),
  sort: { key: 'profit' as SortKey, dir: 'desc' as 'asc' | 'desc' },
  fresh: {} as Record<string, true>, // chegaram ao vivo há pouco: ganham destaque
  highlight: false, // linhas da melhor combinação em destaque
  expanded: null as string | null,
  history: {} as Record<string, History | 'loading' | 'error'>,
  armed: null as string | null, // "Comprar" já clicado uma vez
  frozen: null as string[] | null, // ordem congelada enquanto há compra armada
  buyError: {} as Record<string, string>,
  settingsOpen: false,
  announce: ''
});

const ARM_MS = 4000;
let armTimer: ReturnType<typeof setTimeout> | undefined;

export function rows(): Opportunity[] {
  if (app.frozen) return app.frozen.map((id) => app.opps[id]).filter(Boolean);
  const { key, dir } = app.sort;
  const val = (o: Opportunity) =>
    key === 'profit' ? o.sell.mid.profit_cents
    : key === 'margin' ? o.margin_pct
    : key === 'safe' ? o.sell.quick.profit_cents
    : key === 'eta' ? (o.sell.mid.eta_hours ?? Infinity)
    : o.seen_at;
  const sign = dir === 'asc' ? 1 : -1;
  return Object.values(app.opps).sort((a, b) => sign * (val(a) - val(b)) || b.seen_at - a.seen_at);
}

export function setSort(key: SortKey) {
  app.sort = app.sort.key === key
    ? { key, dir: app.sort.dir === 'asc' ? 'desc' : 'asc' }
    : { key, dir: DEFAULT_DIR[key] };
}

export const purchaseOf = (id: string) => app.purchases.find((p) => p.id === id);

export function arm(id: string) {
  disarm();
  delete app.buyError[id];
  app.armed = id;
  app.frozen = rows().map((o) => o.id); // a linha não foge do dedo durante os 4 s
  armTimer = setTimeout(disarm, ARM_MS);
}

export function disarm() {
  clearTimeout(armTimer);
  app.armed = null;
  app.frozen = null;
}

export async function buy(id: string) {
  disarm();
  try {
    upsertPurchase(await api<Purchase>('/api/buy', 'POST', { id }));
  } catch (e) {
    app.buyError[id] = e instanceof ApiError ? e.message : 'Não consegui comprar.';
  }
}

function upsertPurchase(p: Purchase) {
  const i = app.purchases.findIndex((x) => x.id === p.id);
  const before = i >= 0 ? app.purchases[i].status : null;
  if (i >= 0) app.purchases[i] = p;
  else app.purchases.unshift(p);
  if (before !== p.status) {
    if (p.status === 'bought') say(`Comprado: ${p.name}.`);
    if (p.status === 'failed') say(`A compra de ${p.name} falhou.`);
    if (p.status === 'need_confirmation') say('Confirme a compra no app Steam do celular.');
  }
}

export async function loadHistory(id: string) {
  app.history[id] = 'loading';
  try {
    app.history[id] = await api<History>(`/api/opportunities/${encodeURIComponent(id)}/history`);
  } catch {
    app.history[id] = 'error';
  }
}

export function toggleExpanded(id: string) {
  app.expanded = app.expanded === id ? null : id;
  if (app.expanded === id && !app.history[id]) void loadHistory(id);
}

export async function setSaldo(cents: number | null) {
  app.state = await api<State>('/api/saldo', 'PUT', { cents });
}

export async function saveSettings(patch: Partial<SettingsInput>) {
  app.settings = await api<Settings>('/api/settings', 'PUT', patch);
  void api<State>('/api/state').then((s) => (app.state = s)).catch(() => {});
}

function say(text: string) {
  app.announce = text;
}

function qualifies(o: Opportunity) {
  return o.safe && o.sell.mid.profit_cents >= (app.settings?.notify_min_profit_cents ?? 100);
}

export function notify(o: Opportunity) {
  if (app.settings?.sound) beep();
  if (typeof Notification !== 'undefined' && Notification.permission === 'granted') {
    const n = new Notification(`${signed(o.sell.mid.profit_cents)} de lucro · ${o.name}`, {
      body: `Compre por ${money(o.price_cents)} e venda por ${money(o.sell.mid.price_cents)}.`,
      icon: o.icon_url,
      tag: o.id
    });
    n.onclick = () => window.focus();
  }
  say(`Nova oportunidade: ${o.name}, lucro ${signed(o.sell.mid.profit_cents)}.`);
}

export async function load() {
  app.loadError = null;
  try {
    const [state, opps, settings, purchases] = await Promise.all([
      api<State>('/api/state'),
      api<Opportunity[]>('/api/opportunities'),
      api<Settings>('/api/settings'),
      api<Purchase[]>('/api/purchases')
    ]);
    app.state = state;
    app.opps = Object.fromEntries(opps.map((o) => [o.id, o]));
    app.settings = settings;
    app.purchases = purchases;
    app.loaded = true;
  } catch (e) {
    app.loadError = e instanceof ApiError ? e.message : 'Não consegui carregar os dados.';
  }
}

let es: EventSource | undefined;
let wasDown = false;

function connect() {
  es = new EventSource('/api/events');
  es.onopen = () => {
    app.conn = 'live';
    if (wasDown) void load(); // perdeu eventos enquanto estava fora: recarrega tudo
    wasDown = false;
    say('Conectado de novo.');
  };
  es.onerror = () => {
    if (app.conn !== 'down') say('Sem conexão com o Skin Sniper. Tentando de novo.');
    app.conn = 'down';
    wasDown = true;
    if (es?.readyState === EventSource.CLOSED) setTimeout(connect, 3000);
  };
  es.addEventListener('opportunity', (e) => {
    const o: Opportunity = JSON.parse((e as MessageEvent).data);
    const isNew = !(o.id in app.opps);
    app.opps[o.id] = o;
    if (!isNew) return;
    app.fresh[o.id] = true;
    setTimeout(() => delete app.fresh[o.id], 3500);
    if (app.loaded && qualifies(o)) notify(o);
  });
  es.addEventListener('gone', (e) => {
    const { id } = JSON.parse((e as MessageEvent).data);
    delete app.opps[id];
    if (app.expanded === id) app.expanded = null;
  });
  es.addEventListener('state', (e) => (app.state = JSON.parse((e as MessageEvent).data)));
  es.addEventListener('purchase', (e) => upsertPurchase(JSON.parse((e as MessageEvent).data)));
}

export function start() {
  setInterval(() => (app.now = Math.floor(Date.now() / 1000)), 1000);
  window.addEventListener('pointerdown', unlockAudio, { once: true });
  window.addEventListener('keydown', unlockAudio, { once: true });
  void load();
  connect();
}
