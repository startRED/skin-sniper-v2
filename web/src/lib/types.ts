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
    filters_paused_until: number | null;
    message: string | null;
  };
  combo: { ids: string[]; cost_cents: number; profit_cents: number };
  keys: { steam: KeyStatus; csfloat: KeyStatus; telegram: KeyStatus };
};
export type Tier = { price_cents: number; net_cents: number; profit_cents: number; eta_hours: number | null };
export type Sell = { high: Tier; mid: Tier; quick: Tier };
export type GoneReason = 'unavailable' | 'unprofitable' | 'unwatched' | 'bought' | 'rejected';
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
  why?: string | null;
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
  checked_at?: number | null;
  gone_at?: number | null; // sumiu da Steam: fica na lista de perdidas por um dia
  gone_reason?: GoneReason | null;
  seal?: Seal | null; // raro: "comprovado" ou "ainda não comprovado"
};
export type Seal = {
  proven: boolean;
  sales: number;
  parked: number;
  p25_profit_cents: number | null;
  open_cents: number | null;
  floor_cents: number | null;
  text: string;
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
  wear?: string | null;
  float?: number | null;
  seed?: number | null;
  group?: string | null;
  sell: Sell;
  /** Quanto vale hoje, pela mesma conta da caça; ausente sem estimativa. */
  now?: Sell | null;
};
export type History = { points: [number, number, number][]; sell: Sell };
export type Settings = {
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
  price_samples: number;
  price_error_pct: number | null;
  price_bias_pct: number | null;
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
export type ValueInfo = {
  window_s: number;
  prior: number;
  half_life_h: number;
  life_s: number;
  lives: number;
  reads: number;
  fresh: number;
  classes: { class: string; reads: number; fresh: number }[];
  scored: number;
  loss: number | null;
  base_loss: number | null;
  predicted: number;
  actual: number;
  tuned_at: number;
  policy: ArmScore;
  control: ArmScore;
  catalog_skins: number;
  catalog_seeds: number;
  backtest_n: number;
  backtest_paid: number | null;
  backtest_ratio: number | null;
};
export type ArmScore = { reads: number; fresh: number; worth_cents: number; gain_cents: number };
export type LearningWeek = {
  queries: number;
  finds: number;
  near: number;
  alerts: number;
  alerts_closed: number;
  labeled: number;
  profit_cents: number;
};
export type LearningResearch = {
  reads_1h: number;
  exploratory_1h: number;
  exploratory_limit: number;
  available: number;
  csfloat_1h: number;
  limited_1h: number;
  failed_1h: number;
  followup_1h: number;
};
export type LearningTrial = {
  enabled: boolean;
  pilot: boolean;
  block_id: number | null;
  phase: string;
  base_policy: string;
  reason: string;
};
export type LearningCohort = {
  policy: string | null;
  block_id: number | null;
  kind: string;
  total: number;
  mature: number;
  waiting: number;
  unlabeled: number;
  labeled: number;
  closed: number;
  predicted_cents: number;
  virtual_cents: number;
  indirect: number;
};
export type LearningExploration = {
  candidate: string | null;
  visits: number;
  retained: boolean;
  targets_tested: number;
  unique_finds: number;
};
export type LearningExperiment = {
  blocks: { id: number; pilot: boolean; reads: number; finds: number; predicted_cents: number }[];
  pairs: number;
  mean: number | null;
  lower: number | null;
  conclusion: string;
  reason: string;
};
export type Learning = {
  days: LearningDay[];
  since: number | null;
  teacher: TeacherSummary;
  value: ValueInfo | null;
  refit: number;
  weeks: [LearningWeek, LearningWeek];
  research?: LearningResearch | null;
  trial?: LearningTrial | null;
  cohorts?: LearningCohort[];
  exploration?: LearningExploration | null;
  experiment?: LearningExperiment | null;
  adjustments?: { at: number; old: number | null; new: number | null; why: string }[];
  candidates?: { hours: number; approved: number; over_saldo: number; yellow: number; yellow_gone: number; yellow_fast: number; yellow_relisted: number };
};
