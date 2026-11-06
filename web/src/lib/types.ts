// Contrato da API (docs/projeto.md, "Contrato da API"). Dinheiro em centavos BRL; datas em segundos Unix.
export type KeyStatus = { set: boolean; ok: boolean | null; detail: string | null };
export type State = {
  saldo_cents: number;
  saldo_source: 'steam' | 'manual' | 'none';
  hunt: {
    watching: number;
    queries_per_min: number;
    last_query_at: number | null;
    paused_until: number | null;
    last_ok_at: number | null;
    book_ok_at: number | null;
    message: string | null;
  };
  combo: { ids: string[]; cost_cents: number; profit_cents: number };
  keys: { steam: KeyStatus; csfloat: KeyStatus; telegram: KeyStatus };
};
export type Tier = { price_cents: number; net_cents: number; profit_cents: number; eta_hours: number | null };
export type Sell = { high: Tier; mid: Tier; quick: Tier };
export type Opportunity = {
  id: string;
  name: string;
  icon_url: string;
  steam_url: string;
  inspect_url: string | null;
  wear: string | null;
  float: number | null;
  seed: number | null;
  group: string | null;
  kind: 'desconto' | 'raro';
  price_cents: number;
  sell: Sell;
  margin_pct: number;
  safe: boolean;
  confidence: 'alta' | 'media' | 'baixa';
  ev_profit_cents: number | null;
  days_locked: number | null;
  profit_per_day_cents: number | null;
  risk: 'baixo' | 'medio' | 'alto' | null;
  evidence: string;
  seen_at: number;
  gone_at?: number | null; // sumiu da Steam: fica na lista de perdidas por um dia
};
export type Purchase = {
  id: string;
  name: string;
  icon_url: string;
  price_cents: number;
  status: 'pending' | 'need_confirmation' | 'bought' | 'failed';
  message: string | null;
  bought_at: number | null;
  tradable_at: number | null;
  kind?: 'desconto' | 'raro' | null;
  sold_cents?: number | null;
  sell: Sell;
};
export type History = { points: [number, number, number][]; sell: Sell };
export type Settings = {
  steam_login_secure: string | null;
  csfloat_key: string | null;
  telegram_token: string | null;
  telegram_chat_id: string | null;
  panel_url: string | null;
  sound: boolean;
  notify_min_profit_cents: number;
};
export type SettingsInput = { [K in keyof Settings]: Settings[K] | null };
export type WatchRow = {
  name: string;
  icon_url: string;
  floor_cents: number | null;
  max_buy_cents: number | null; // ≤ 0 ou null: sem preço que dê lucro / sem estimativa
  sales_per_day: number;
  kind: 'desconto' | 'raro';
  group: string | null;
  last_checked_at: number | null;
  mode: 'barato' | 'seed' | 'float';
  score: number;
  reason: string;
  bid_cents: number | null; // pedido de compra sugerido (1 centavo acima do maior); null quando não compensa
  bid_profit_cents: number | null; // lucro no preço médio se o pedido encher
  book_at: number | null;
};
// GET /api/learning: placar do aprendizado, um dia de Brasília por linha (do mais recente).
export type LearningDay = {
  day: string;
  queries: number;
  books: number;
  new_listings: number;
  aged: number;
  age_median_min: number | null;
  sales: number;
  relists: number;
  life_median_min: number | null;
  near: number;
  finds: number;
};
export type TeacherSummary = {
  ghosts: number;
  waiting: number;
  fast_alerts: number;
  fast_alerts_closed: number;
  fast_near: number;
  fast_near_closed: number;
  fast_ratio: number | null;
  final_alerts: number;
  final_alerts_won: number;
  profit_cents: number;
  predicted_cents: number;
  unlabeled: number;
  rare_labeled: number;
  rare_closed: number;
};
export type Learning = { days: LearningDay[]; since: number | null; teacher: TeacherSummary };
