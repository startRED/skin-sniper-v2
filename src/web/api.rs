//! web/api — tipos do contrato (docs/projeto.md) e as rotas `/api/*`.
//! Dinheiro em centavos inteiros; datas em segundos Unix.

use super::AppState;
use axum::Json;
use axum::extract::{Path, State as AxState};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SaldoSource {
    Steam,
    Manual,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hunt {
    pub watching: u32,
    pub queries_per_min: f64,
    pub last_query_at: Option<i64>,
    pub paused_until: Option<i64>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Combo {
    pub ids: Vec<String>,
    pub cost_cents: i64,
    pub profit_cents: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyStatus {
    pub set: bool,
    pub ok: Option<bool>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keys {
    pub steam: KeyStatus,
    pub csfloat: KeyStatus,
    pub telegram: KeyStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub saldo_cents: i64,
    pub saldo_source: SaldoSource,
    pub hunt: Hunt,
    pub combo: Combo,
    pub keys: Keys,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tier {
    pub price_cents: i64,
    pub net_cents: i64,
    pub profit_cents: i64,
    pub eta_hours: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sell {
    pub high: Tier,
    pub mid: Tier,
    pub quick: Tier,
    /// Fator das vendas reais já aplicado a estes preços (1 = nenhum); a calibração compara sem ele.
    #[serde(default = "one")]
    pub correction: f64,
}

fn one() -> f64 {
    1.0
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Desconto,
    Raro,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Alta,
    Media,
    Baixa,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Baixo,
    Medio,
    Alto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opportunity {
    pub id: String,
    pub name: String,
    pub icon_url: String,
    pub steam_url: String,
    pub inspect_url: Option<String>,
    pub wear: Option<String>,
    pub float: Option<f64>,
    pub seed: Option<i64>,
    pub group: Option<String>,
    pub kind: Kind,
    pub price_cents: i64,
    pub sell: Sell,
    pub margin_pct: f64,
    pub safe: bool,
    pub confidence: Confidence,
    #[serde(default)]
    pub ev_profit_cents: Option<i64>,
    #[serde(default)]
    pub days_locked: Option<f64>,
    #[serde(default)]
    pub profit_per_day_cents: Option<i64>,
    #[serde(default)]
    pub risk: Option<Risk>,
    pub evidence: String,
    pub seen_at: i64,
    /// Quando o anúncio sumiu (vendido para outro, comprado por você ou fora da seleção). Vem da
    /// coluna `gone_at`, não do JSON guardado.
    #[serde(default)]
    pub gone_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PurchaseStatus {
    Pending,
    NeedConfirmation,
    Bought,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Purchase {
    pub id: String,
    pub name: String,
    pub icon_url: String,
    pub price_cents: i64,
    pub status: PurchaseStatus,
    pub message: Option<String>,
    pub bought_at: Option<i64>,
    pub tradable_at: Option<i64>,
    pub sell: Sell,
    #[serde(default)]
    pub kind: Option<Kind>,
    #[serde(default)]
    pub sold_cents: Option<i64>,
}

/// Resposta de `GET /api/opportunities/:id/history`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct History {
    pub points: Vec<(i64, i64, i64)>,
    pub sell: Sell,
}

/// Uma linha de `GET /api/watch`: o que a caça está vigiando.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchRow {
    pub name: String,
    pub icon_url: String,
    /// Menor anúncio visto na última consulta; `null` se ainda não consultou ou a página veio vazia.
    pub floor_cents: Option<i64>,
    /// Maior preço de compra que ainda daria lucro ≥ R$ 0,20 e ≥ 5 % no preço médio; `null` sem estimativa
    /// (histórico curto) e sempre `null` em item raro.
    pub max_buy_cents: Option<i64>,
    pub sales_per_day: f64,
    pub kind: Kind,
    pub group: Option<String>,
    pub last_checked_at: Option<i64>,
    /// Modo de busca do alvo: `barato` (menores preços), `seed` (faixas raras) ou `float` (float baixo).
    pub mode: String,
    /// Lucro esperado por consulta gasta, em centavos, aprendido sozinho (0 enquanto não houver o que medir).
    pub score: f64,
    /// Por que a agenda dá (ou não) consultas a este alvo, em palavras curtas: "rendeu R$ 1,20 em 30
    /// consultas", "testando alvo novo", "quase deu lucro 3 vezes", "sem achar nada há 5 h".
    pub reason: String,
}

/// Leitura de `Settings`: segredos já mascarados (`••••gLO9`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    pub steam_login_secure: Option<String>,
    pub csfloat_key: Option<String>,
    pub telegram_token: Option<String>,
    pub telegram_chat_id: Option<String>,
    pub panel_url: Option<String>,
    pub sound: bool,
    pub notify_min_profit_cents: i64,
}

/// `Partial<SettingsInput>`: campo ausente = não mexe; `null` = apaga (volta ao padrão).
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsInput {
    #[serde(default, deserialize_with = "present")]
    pub steam_login_secure: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub csfloat_key: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub telegram_token: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub telegram_chat_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub panel_url: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub sound: Option<Option<bool>>,
    #[serde(default, deserialize_with = "present")]
    pub notify_min_profit_cents: Option<Option<i64>>,
}

/// Distingue campo ausente (`None`) de `null` explícito (`Some(None)`).
fn present<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// `Debug` sem valores: só diz se cada campo seria mexido.
impl fmt::Debug for SettingsInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn s<T>(v: &Option<Option<T>>) -> &'static str {
            match v {
                None => "unchanged",
                Some(None) => "clear",
                Some(Some(_)) => "set(••••)",
            }
        }
        f.debug_struct("SettingsInput")
            .field("steam_login_secure", &s(&self.steam_login_secure))
            .field("csfloat_key", &s(&self.csfloat_key))
            .field("telegram_token", &s(&self.telegram_token))
            .field("telegram_chat_id", &s(&self.telegram_chat_id))
            .field("panel_url", &s(&self.panel_url))
            .field("sound", &s(&self.sound))
            .field("notify_min_profit_cents", &s(&self.notify_min_profit_cents))
            .finish()
    }
}

/// Eventos do SSE; outros módulos publicam em `AppState::events`.
#[derive(Debug, Clone)]
pub enum Event {
    Opportunity(Opportunity),
    Gone { id: String },
    State(State),
    Purchase(Purchase),
}

pub struct ApiError(pub(crate) StatusCode, pub(crate) String);

impl ApiError {
    pub fn new(code: StatusCode, msg: impl Into<String>) -> Self {
        ApiError(code, msg.into())
    }
    fn not_wired(what: &str) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("{what}: ainda não ligado"),
        )
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!("erro interno: {e:#}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "erro interno")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

pub async fn get_state(AxState(app): AxState<AppState>) -> ApiResult<State> {
    Ok(Json(app.current_state().await?))
}

#[derive(Deserialize)]
pub struct SaldoInput {
    cents: Option<i64>,
}

pub async fn put_saldo(
    AxState(app): AxState<AppState>,
    Json(body): Json<SaldoInput>,
) -> ApiResult<State> {
    if body.cents.is_some_and(|c| c < 0) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "cents não pode ser negativo",
        ));
    }
    app.settings.set_saldo(body.cents).await?;
    if let Some(rt) = &app.rt {
        rt.settings_changed().await?;
    }
    let state = app.current_state().await?;
    let _ = app.events.send(Event::State(state.clone()));
    Ok(Json(state))
}

/// Linhas `json` do banco; as que não leem no formato do contrato são puladas (e avisadas).
fn parse_rows<T: serde::de::DeserializeOwned>(what: &str, rows: Vec<String>) -> Vec<T> {
    rows.iter()
        .filter_map(|j| match serde_json::from_str(j) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("{what} no banco fora do contrato: {e}");
                None
            }
        })
        .collect()
}

/// Perdidas ficam visíveis por um dia, para o usuário ver o que escapou.
const LOST_KEEP_SECS: i64 = 86_400;
const LOST_MAX: i64 = 50;

/// Ativas e, depois, as perdidas mais recentes (com `gone_at`).
pub async fn list_opportunities(AxState(app): AxState<AppState>) -> ApiResult<Vec<Opportunity>> {
    let since = crate::db::now_unix() - LOST_KEEP_SECS;
    let rows = app
        .db
        .call(move |c| {
            c.prepare(
                "SELECT json, gone_at FROM opportunities WHERE gone_at IS NULL
                 UNION ALL SELECT * FROM (SELECT json, gone_at FROM opportunities
                   WHERE gone_at >= ?1 ORDER BY gone_at DESC LIMIT ?2)",
            )?
            .query_map([since, LOST_MAX], |r| {
                Ok((r.get::<_, String>(0)?, r.get(1)?))
            })?
            .collect::<rusqlite::Result<Vec<(String, Option<i64>)>>>()
        })
        .await?;
    Ok(Json(
        rows.into_iter()
            .filter_map(|(j, gone_at)| {
                let mut o: Opportunity = parse_rows("oportunidade", vec![j]).pop()?;
                o.gone_at = gone_at;
                Some(o)
            })
            .collect(),
    ))
}

pub async fn opportunity_history(
    AxState(app): AxState<AppState>,
    Path(id): Path<String>,
) -> ApiResult<History> {
    let Some(rt) = &app.rt else {
        return Err(ApiError::not_wired("histórico (precificação)"));
    };
    Ok(Json(rt.history(&id).await?))
}

#[derive(Deserialize)]
pub struct BuyInput {
    pub id: String,
}

pub async fn watch(AxState(app): AxState<AppState>) -> ApiResult<Vec<WatchRow>> {
    let Some(rt) = &app.rt else {
        return Err(ApiError::not_wired("vigilância"));
    };
    Ok(Json(crate::hunt::watch::list(&rt.hunt).await?))
}

pub async fn buy(
    AxState(app): AxState<AppState>,
    Json(body): Json<BuyInput>,
) -> ApiResult<Purchase> {
    let Some(rt) = &app.rt else {
        return Err(ApiError::not_wired("compra"));
    };
    Ok(Json(rt.buy(&body.id).await?))
}

pub async fn list_purchases(AxState(app): AxState<AppState>) -> ApiResult<Vec<Purchase>> {
    let rows = app.db.call(|c| {
        c.prepare("SELECT p.json, (SELECT sold_cents FROM outcomes WHERE purchase_id = p.id ORDER BY rowid DESC LIMIT 1) FROM purchases p ORDER BY p.created_at DESC")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()
    }).await?;
    Ok(Json(
        rows.into_iter()
            .flat_map(|(json, sold)| {
                let mut purchases: Vec<Purchase> = parse_rows("compra", vec![json]);
                for p in &mut purchases {
                    p.sold_cents = sold;
                }
                purchases
            })
            .collect(),
    ))
}

pub async fn get_settings(AxState(app): AxState<AppState>) -> ApiResult<Settings> {
    Ok(Json(app.settings.view().await?))
}

pub async fn put_settings(
    AxState(app): AxState<AppState>,
    Json(input): Json<SettingsInput>,
) -> ApiResult<Settings> {
    let settings = app.settings.apply(input).await.map_err(|e| {
        match e.downcast::<crate::settings::Invalid>() {
            Ok(inv) => ApiError::new(StatusCode::BAD_REQUEST, inv.to_string()),
            Err(e) => e.into(),
        }
    })?;
    if let Some(rt) = &app.rt {
        rt.settings_changed().await?;
    }
    // Painel e Telegram mostram o estado das chaves; empurra o novo.
    if let Ok(state) = app.current_state().await {
        let _ = app.events.send(Event::State(state));
    }
    Ok(Json(settings))
}

pub async fn api_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "rota inexistente")
}
