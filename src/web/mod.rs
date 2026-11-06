//! web — axum: rotas do contrato, SSE, guarda de Host e arquivos embutidos.

pub mod api;
pub mod calibration;
pub mod guard;
pub mod login;
pub mod sse;

use crate::db::Db;
use crate::settings::SettingsStore;
use anyhow::Result;
use api::{Combo, Event, Hunt, KeyStatus, Keys, SaldoSource, State};
use axum::Router;
use axum::http::header;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{any, get, post, put};
use rust_embed::RustEmbed;
use tokio::sync::broadcast;

#[derive(RustEmbed)]
#[folder = "web/dist"]
struct Assets;

const NO_BUILD_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Skin Sniper</title>\
<body style=\"font-family:sans-serif;margin:3rem\"><h1>Painel não compilado</h1>\
<p>Rode <code>cd web &amp;&amp; npm ci &amp;&amp; npm run build</code> e depois <code>cargo build</code> de novo.</p>";

/// O que as rotas compartilham. Outros módulos recebem um clone e publicam em `events`.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub settings: SettingsStore,
    pub events: broadcast::Sender<Event>,
    pub pc_name: Option<String>,
    /// Caça e compra ligadas; `None` nos testes de rota, que usam só o banco.
    pub rt: Option<std::sync::Arc<crate::runtime::Runtime>>,
    pub login: std::sync::Arc<crate::steam::auth::Login>,
}

impl AppState {
    pub fn new(db: Db, settings: SettingsStore) -> Self {
        AppState {
            db,
            login: crate::steam::auth::Login::new(settings.clone()),
            settings,
            events: broadcast::channel(256).0,
            pc_name: guard::pc_name(),
            rt: None,
        }
    }

    /// Só o que existe hoje: saldo manual e quais chaves estão salvas. Caça e combo ficam vazios
    /// (com mensagem) até os módulos correspondentes serem ligados.
    pub async fn current_state(&self) -> Result<State> {
        if let Some(rt) = &self.rt {
            return Ok(rt.hunt.state());
        }
        let saldo = self.settings.saldo().await?;
        let s = self.settings.view().await?;
        let key = |set: bool| KeyStatus {
            set,
            ok: None,
            detail: None,
        };
        Ok(State {
            saldo_cents: saldo.unwrap_or(0),
            saldo_source: if saldo.is_some() {
                SaldoSource::Manual
            } else {
                SaldoSource::None
            },
            hunt: Hunt {
                watching: 0,
                queries_per_min: 0.0,
                last_query_at: None,
                paused_until: None,
                last_ok_at: None,
                book_ok_at: None,
                message: Some("caça ainda não ligada".into()),
            },
            combo: Combo {
                ids: vec![],
                cost_cents: 0,
                profit_cents: 0,
            },
            keys: Keys {
                steam: key(s.steam_login_secure.is_some()),
                csfloat: key(s.csfloat_key.is_some()),
                telegram: key(s.telegram_token.is_some()),
            },
        })
    }
}

pub fn router(app: AppState) -> Router {
    let pc = app.pc_name.clone();
    Router::new()
        .route("/api/state", get(api::get_state))
        .route("/api/saldo", put(api::put_saldo))
        .route("/api/opportunities", get(api::list_opportunities))
        .route(
            "/api/opportunities/{id}/history",
            get(api::opportunity_history),
        )
        .route("/api/watch", get(api::watch))
        .route("/api/buy", post(api::buy))
        .route("/api/purchases", get(api::list_purchases))
        .route("/api/purchases/{id}/sold", post(calibration::sold))
        .route("/api/calibration", get(calibration::get))
        .route(
            "/api/settings",
            get(api::get_settings).put(api::put_settings),
        )
        .route("/api/steam/login/qr", post(login::start))
        .route("/api/steam/login/status", get(login::status))
        .route("/api/events", get(sse::events))
        .route("/api/{*rest}", any(api::api_not_found))
        .fallback(static_files)
        .layer(axum::middleware::from_fn_with_state(pc, guard::check))
        .with_state(app)
}

/// Tipo pelo sufixo: o que um build do Vite gera; sem crate de mime só por isso.
fn mime(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Arquivo embutido; rota desconhecida cai no `index.html` (SPA).
async fn static_files(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let found = Assets::get(path)
        .map(|f| (path, f))
        .or_else(|| Assets::get("index.html").map(|f| ("index.html", f)));
    match found {
        Some((name, f)) => ([(header::CONTENT_TYPE, mime(name))], f.data).into_response(),
        None => Html(NO_BUILD_PAGE).into_response(),
    }
}

#[cfg(test)]
mod tests;
