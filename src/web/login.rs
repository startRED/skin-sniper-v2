//! Rotas antigas de login: o aplicativo usa apenas consultas públicas.
use super::api::ApiError;
use axum::http::StatusCode;

pub async fn start() -> ApiError {
    ApiError::new(
        StatusCode::GONE,
        "Login Steam desativado; o aplicativo usa apenas consultas públicas",
    )
}

pub async fn status() -> ApiError {
    ApiError::new(
        StatusCode::GONE,
        "Login Steam desativado; o aplicativo usa apenas consultas públicas",
    )
}
