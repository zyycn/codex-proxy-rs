use axum::{
    extract::{Query, State},
    http::{
        header::{HeaderValue, SET_COOKIE},
        HeaderMap, StatusCode,
    },
    response::{IntoResponse, Response},
    Extension, Json,
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    auth::admin_session::verify_admin_password,
    http::{auth::admin_session_id, middleware::RequestId},
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
        // body code 给前端做业务分支，HTTP status 仍然是传输层真相。
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    pub password: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginData {
    pub expires_at: String,
}

pub async fn login(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Json(payload): Json<LoginRequest>,
) -> Response {
    let request_id = request_id.as_str().to_string();
    let Some(pool) = state.db() else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Database is not initialized", (), request_id),
        )
        .into_response();
    };

    let admin = match load_first_admin(pool).await {
        Ok(Some(admin)) => admin,
        Ok(None) => {
            return AdminResponse::new(
                StatusCode::UNAUTHORIZED,
                AdminEnvelope::new(40102, "Admin password invalid", (), request_id),
            )
            .into_response();
        }
        Err(_) => {
            return AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to load admin user", (), request_id),
            )
            .into_response();
        }
    };

    // admin 登录只校验管理员密码；客户端 cpr_ API Key 不能参与后台登录。
    match verify_admin_password(&payload.password, &admin.password_hash) {
        Ok(true) => {}
        Ok(false) => {
            return AdminResponse::new(
                StatusCode::UNAUTHORIZED,
                AdminEnvelope::new(40102, "Admin password invalid", (), request_id),
            )
            .into_response();
        }
        Err(_) => {
            return AdminResponse::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                AdminEnvelope::new(50001, "Failed to verify admin password", (), request_id),
            )
            .into_response();
        }
    }

    let ttl_minutes = state.config().admin.session_ttl_minutes;
    let Ok(ttl_minutes_i64) = i64::try_from(ttl_minutes) else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Admin session ttl is invalid", (), request_id),
        )
        .into_response();
    };
    let expires_at = Utc::now() + Duration::minutes(ttl_minutes_i64);
    let session_id = format!("sess_{}", Uuid::new_v4().simple());
    if create_admin_session(pool, &session_id, &admin.id, expires_at)
        .await
        .is_err()
    {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(50001, "Failed to create admin session", (), request_id),
        )
        .into_response();
    }

    let Some(cookie) = admin_session_set_cookie(&session_id, ttl_minutes) else {
        return AdminResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            AdminEnvelope::new(
                50001,
                "Failed to create admin session cookie",
                (),
                request_id,
            ),
        )
        .into_response();
    };
    let mut response = AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(
            LoginData {
                expires_at: expires_at.to_rfc3339(),
            },
            request_id,
        ),
    )
    .into_response();
    response.headers_mut().insert(SET_COOKIE, cookie);
    response
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

#[derive(Debug)]
struct AdminUserRow {
    id: String,
    password_hash: String,
}

async fn load_first_admin(pool: &sqlx::SqlitePool) -> Result<Option<AdminUserRow>, sqlx::Error> {
    let row =
        sqlx::query("select id, password_hash from admin_users order by created_at asc limit 1")
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|row| AdminUserRow {
        id: row.get("id"),
        password_hash: row.get("password_hash"),
    }))
}

async fn create_admin_session(
    pool: &sqlx::SqlitePool,
    session_id: &str,
    user_id: &str,
    expires_at: chrono::DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "insert into admin_sessions (id, user_id, expires_at, created_at) values (?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind(user_id)
    .bind(expires_at.to_rfc3339())
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

fn admin_session_set_cookie(session_id: &str, ttl_minutes: u64) -> Option<HeaderValue> {
    let max_age = ttl_minutes.checked_mul(60)?;
    let cookie = format!(
        "cpr_admin_session={session_id}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}"
    );
    HeaderValue::from_str(&cookie).ok()
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
    admin_session_id(headers)
}
