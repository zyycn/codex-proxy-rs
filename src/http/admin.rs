use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{
    http::middleware::RequestId,
    pagination::{clamp_limit, Page},
    state::AppState,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminEnvelope<T> {
    pub code: u32,
    pub message: String,
    pub data: T,
    pub request_id: String,
}

impl<T> AdminEnvelope<T> {
    pub fn new(
        code: u32,
        message: impl Into<String>,
        data: T,
        request_id: impl Into<String>,
    ) -> Self {
        // 中文注释：body code 给前端做业务分支，HTTP status 仍然是传输层真相。
        Self {
            code,
            message: message.into(),
            data,
            request_id: request_id.into(),
        }
    }

    pub fn ok(data: T, request_id: impl Into<String>) -> Self {
        Self::new(200, "OK", data, request_id)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageMeta {
    pub limit: u32,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminPageEnvelope<T> {
    pub code: u32,
    pub message: String,
    pub data: Vec<T>,
    pub page: PageMeta,
    pub request_id: String,
}

impl<T> AdminPageEnvelope<T> {
    pub fn new(
        code: u32,
        message: impl Into<String>,
        page: Page<T>,
        limit: u32,
        request_id: impl Into<String>,
    ) -> Self {
        let Page { items, next_cursor } = page;
        Self {
            code,
            message: message.into(),
            data: items,
            page: PageMeta { limit, next_cursor },
            request_id: request_id.into(),
        }
    }

    pub fn ok(page: Page<T>, limit: u32, request_id: impl Into<String>) -> Self {
        Self::new(200, "OK", page, limit, request_id)
    }
}

#[derive(Debug, Clone)]
pub struct AdminResponse<T> {
    pub status: StatusCode,
    pub body: T,
}

impl<T> AdminResponse<T> {
    pub fn new(status: StatusCode, body: T) -> Self {
        Self { status, body }
    }
}

impl<T> IntoResponse for AdminResponse<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

pub async fn logs(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<LogsQuery>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };
    match validate_admin_session(pool, &headers).await {
        Ok(true) => {}
        Ok(false) => {
            return AdminResponse::new(
                StatusCode::UNAUTHORIZED,
                AdminEnvelope::new(40101, "Admin session required", (), request_id),
            )
            .into_response();
        }
        Err(_) => {
            return AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to validate admin session", (), request_id),
            )
            .into_response();
        }
    }

    let limit = clamp_limit(query.limit.unwrap_or(50));
    let Some(repo) = state.event_logs() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Event log repository is not initialized",
                (),
                request_id,
            ),
        )
        .into_response();
    };
    match repo.list(query.cursor, limit).await {
        Ok(page) => AdminResponse::new(
            StatusCode::OK,
            AdminPageEnvelope::ok(page, limit, request_id),
        )
        .into_response(),
        Err(_) => AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to list event logs", (), request_id),
        )
        .into_response(),
    }
}

async fn validate_admin_session(
    pool: &sqlx::SqlitePool,
    headers: &HeaderMap,
) -> Result<bool, sqlx::Error> {
    let Some(session_id) = admin_session_cookie(headers) else {
        return Ok(false);
    };
    let now = Utc::now().to_rfc3339();
    let count: (i64,) =
        sqlx::query_as("select count(*) from admin_sessions where id = ? and expires_at > ?")
            .bind(session_id)
            .bind(now)
            .fetch_one(pool)
            .await?;
    Ok(count.0 > 0)
}

fn admin_session_cookie(headers: &HeaderMap) -> Option<&str> {
    let cookie = headers.get("cookie")?.to_str().ok()?;
    cookie.split(';').map(str::trim).find_map(|part| {
        let (name, value) = part.split_once('=')?;
        (name == "cpr_admin_session" && !value.is_empty()).then_some(value)
    })
}
