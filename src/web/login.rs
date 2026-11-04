//! web/login — login da Steam pelo QR do app (`steam::auth`).

use super::AppState;
use super::api::{ApiError, Event};
use crate::steam::auth::{QrStart, Status};
use axum::Json;
use axum::extract::State as AxState;
use axum::http::StatusCode;

pub async fn start(AxState(app): AxState<AppState>) -> Result<Json<QrStart>, ApiError> {
    app.login.start_qr().await.map(Json).map_err(|e| {
        tracing::warn!("login por QR: {e:#}");
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "A Steam não aceitou o pedido do QR agora. Tente de novo em instantes.",
        )
    })
}

/// Pergunta à Steam se o QR já foi aprovado. Ao concluir, o cookie novo vale na hora.
pub async fn status(AxState(app): AxState<AppState>) -> Result<Json<Status>, ApiError> {
    let (status, saved) = app.login.poll().await;
    if saved {
        if let Some(rt) = &app.rt {
            rt.settings_changed().await?;
        }
        if let Ok(state) = app.current_state().await {
            let _ = app.events.send(Event::State(state));
        }
    }
    Ok(Json(status))
}
