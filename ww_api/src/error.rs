//! API error type: maps domain / database failures onto HTTP status codes.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("missing or invalid bearer token")]
    Unauthorized,
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unprocessable(String),
    /// Details are logged, never returned to the client.
    #[error("internal error")]
    Internal(String),
}

impl ApiError {
    pub fn internal(e: impl std::fmt::Display) -> Self {
        ApiError::Internal(e.to_string())
    }
}

impl From<tokio_postgres::Error> for ApiError {
    fn from(e: tokio_postgres::Error) -> Self {
        if let Some(db) = e.as_db_error() {
            match db.code().code() {
                // unique_violation
                "23505" => return ApiError::Conflict(format!("already exists: {}", db.message())),
                // foreign_key_violation
                "23503" => {
                    return ApiError::Conflict(format!("referential integrity: {}", db.message()))
                }
                // check_violation
                "23514" => return ApiError::Unprocessable(format!("invalid value: {}", db.message())),
                // restrict_violation (audit_log append-only trigger)
                "23001" => return ApiError::Conflict(db.message().to_string()),
                _ => {}
            }
        }
        ApiError::Internal(e.to_string())
    }
}

impl From<deadpool_postgres::PoolError> for ApiError {
    fn from(e: deadpool_postgres::PoolError) -> Self {
        ApiError::Internal(format!("pool: {e}"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, m.clone()),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, m.clone()),
            ApiError::Unprocessable(m) => (StatusCode::UNPROCESSABLE_ENTITY, m.clone()),
            ApiError::Internal(detail) => {
                tracing::error!(%detail, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string())
            }
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
