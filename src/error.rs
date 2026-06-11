use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use thiserror::Error;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("upstream error: {0}")]
    Upstream(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("bad request: {0}")]
    BadRequest(String),
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: ErrorMessage<'a>,
}

#[derive(Serialize)]
struct ErrorMessage<'a> {
    message: &'a str,
    #[serde(rename = "type")]
    kind: &'a str,
    code: &'a str,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, kind) = match &self {
            Self::BadRequest(_) | Self::Config(_) => (
                StatusCode::BAD_REQUEST,
                "bad_request",
                "invalid_request_error",
            ),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "invalid_request_error",
            ),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden", "invalid_request_error"),
            Self::Database(_) | Self::Upstream(_) => {
                (StatusCode::BAD_GATEWAY, "upstream_error", "server_error")
            }
        };

        let message = self.to_string();
        let body = ErrorBody {
            error: ErrorMessage {
                message: &message,
                kind,
                code,
            },
        };

        (status, Json(body)).into_response()
    }
}
