import { api, ApiError } from './api';
import { money, signed } from './format';
import { beep, unlockAudio } from './sound';
import type { History, Opportunity, Purchase, Settings, SettingsInput, State, WatchRow } from './types';

export type SortKey = 'profit' | 'margin' | 'safe' | 'eta' | 'new';
const DEFAULT_DIR: Record<SortKey, 'asc' | 'desc'> = { profit: 'desc', margin: 'desc', safe: 'desc', eta: 'asc', new: 'desc' };

export const app = $state({
  loaded: false,
  loadError: null as string | null,
  warnings: [] as string[], // partes que o servidor ainda não liga (503) ou que falharam, sem derrubar a tela
  conn: 'connecting' as 'connecting' | 'live' | 'down',
  state: null as State | null,
  opps: {} as Record<string, Opportunity>,
  settings: null as Settings | null,
  purchases: [] as Purchase[],
  watch: [] as WatchRow[], // o que a caça vigia (ordem do servidor: mais perto de dar lucro primeiro)
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

const LOST_KEEP_S = 86400;

// Perdidas: sumiram da Steam no último dia. A mais recente primeiro.
export function lostRows(): Opportunity[] {
  return Object.values(app.opps)
    .filter((o) => o.gone_at && app.now - o.gone_at < LOST_KEEP_S)
    .sort((a, b) => b.gone_at! - a.gone_at!);
}

export function rows(): Opportunity[] {
  if (app.frozen) return app.frozen.map((id) => app.opps[id]).filter((o) => o && !o.gone_at);
  const { key, dir } = app.sort;
  const val = (o: Opportunity) =>
    key === 'profit' ? o.sell.mid.profit_cents
    : key === 'margin' ? o.margin_pct
    : key === 'safe' ? o.sell.quick.profit_cents
    : key === 'eta' ? (o.sell.mid.eta_hours ?? Infinity)
    : o.seen_at;
  const sign = dir === 'asc' ? 1 : -1;
  return Object.values(app.opps).filter((o) => !o.gone_at).sort((a, b) => sign * (val(a) - val(b)) || b.seen_at - a.seen_at);
}

export function setSort(key: SortKey) {
  app.sort = app.sort.key === key
    ? { key, dir: app.sort.dir === 'asc' ? 'desc' : 'asc' }
    : { key, dir: DEFAULT_DIR[key] };
}

// A Steam não confirmou: pode ter comprado ou não. Nunca oferecer tentar de novo.
export const isUncertain = (p?: Purchase) => p?.status === 'failed' && /invent[aá]rio/i.test(p.message ?? '');

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
    if (isUncertain(p)) say(`Não deu para confirmar a compra de ${p.name}. Confira seu inventário da Steam.`);
    else if (p.status === 'failed') say(`A compra de ${p.name} falhou.`);
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

let watchBusy = false;
export async function loadWatch() {
  if (watchBusy) return;
  watchBusy = true;
  try {
    app.watch = await api<WatchRow[]>('/api/watch');
  } catch {} // fica a lista anterior; a próxima tentativa vem em 30 s ou no próximo estado
  watchBusy = false;
}

const reason = (r: PromiseRejectedResult) => (r.reason instanceof ApiError ? r.reason.message : 'Não consegui carregar uma parte dos dados.');

// Devolve true se o essencial (estado) carregou. As outras rotas falhando só viram aviso.
export async function load(): Promise<boolean> {
  const [state, opps, settings, purchases] = await Promise.allSettled([
    api<State>('/api/state'),
    api<Opportunity[]>('/api/opportunities'),
    api<Settings>('/api/settings'),
    api<Purchase[]>('/api/purchases')
  ]);
  if (state.status === 'rejected') {
    if (!app.loaded) app.loadError = reason(state);
    return false;
  }
  app.loadError = null;
  app.warnings = [opps, settings, purchases].filter((r) => r.status === 'rejected').map(reason);
  app.state = state.value;
  if (opps.status === 'fulfilled') {
    disarm(); // a linha armada pode ter sumido e voltado enquanto estava desconectado
    app.opps = Object.fromEntries(opps.value.map((o) => [o.id, o]));
  }
  if (settings.status === 'fulfilled') app.settings = settings.value;
  if (purchases.status === 'fulfilled') app.purchases = purchases.value;
  app.loaded = true;
  return true;
}

let es: EventSource | undefined;
let wasDown = false;

// Depois de uma queda, eventos novos esperam a recarga terminar: senão a recarga atropelaria o que acabou de chegar.
let queue: (() => void)[] | null = null;
const apply = (fn: () => void) => (queue ? queue.push(fn) : fn());

async function resync() {
  queue = [];
  while (!(await load())) await new Promise((r) => setTimeout(r, 3000));
  const q = queue;
  queue = null;
  q.forEach((fn) => fn());
  wasDown = false;
}

function connect() {
  es = new EventSource('/api/events');
  es.onopen = () => {
    app.conn = 'live';
    say('Conectado de novo.');
    if (wasDown && !queue) void resync(); // o servidor encerra o stream de quem atrasa: perdemos eventos, recarrega tudo
  };
  es.onerror = () => {
    if (app.conn !== 'down') say('Sem conexão com o Skin Sniper. Tentando de novo.');
    app.conn = 'down';
    wasDown = true;
    if (es?.readyState === EventSource.CLOSED) setTimeout(connect, 3000);
  };
  const on = (name: string, fn: (data: any) => void) =>
    es!.addEventListener(name, (e) => {
      const data = JSON.parse((e as MessageEvent).data);
      apply(() => fn(data));
    });
  on('opportunity', (o: Opportunity) => {
    const isNew = !app.opps[o.id] || !!app.opps[o.id].gone_at;
    app.opps[o.id] = o;
    if (!isNew) return;
    app.fresh[o.id] = true;
    setTimeout(() => delete app.fresh[o.id], 3500);
    if (app.loaded && qualifies(o)) notify(o);
  });
  on('gone', ({ id }) => {
    const o = app.opps[id];
    if (o) app.opps[id] = { ...o, gone_at: app.now };
    if (app.armed === id) disarm();
  });
  on('state', (s: State) => {
    app.state = s;
    void loadWatch();
  });
  on('purchase', (p: Purchase) => upsertPurchase(p));
}

export function start() {
  setInterval(() => (app.now = Math.floor(Date.now() / 1000)), 1000);
  window.addEventListener('pointerdown', unlockAudio, { once: true });
  window.addEventListener('keydown', unlockAudio, { once: true });
  void load();
  void loadWatch();
  setInterval(loadWatch, 30000);
  connect();
}
