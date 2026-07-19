//! 系统管理接口的查询与请求 wire contract。
//!
//! 这里不依赖更新服务或进程控制；应用层通过窄端口提供系统操作事实。

use std::{convert::Infallible, pin::Pin};

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::Value;

use super::{AdminAuth, AdminEnvelope, AdminError, AdminResponse, AdminSessionState};

/// 更新详情查询参数。
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateDetailQuery {
    refresh: Option<bool>,
}

impl UpdateDetailQuery {
    /// 是否强制从发布源刷新。
    #[must_use]
    pub fn refresh(&self) -> bool {
        self.refresh.unwrap_or(false)
    }
}

/// 执行更新请求。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateRequest {
    target_version: String,
}

impl UpdateRequest {
    /// 取出确认的目标版本原值，由更新领域负责版本规范化和校验。
    #[must_use]
    pub fn into_target_version(self) -> String {
        self.target_version
    }
}

/// 一条已经脱敏的更新事件。
pub struct SystemUpdateEvent {
    pub id: String,
    pub data: Value,
}

/// 每个 SSE 订阅独占的更新事件流。
pub type SystemUpdateEventStream = Pin<Box<dyn Stream<Item = SystemUpdateEvent> + Send + 'static>>;

/// 系统操作失败分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemAdminErrorKind {
    Invalid,
    Conflict,
    BadGateway,
    Internal,
}

/// 系统操作安全错误。
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct SystemAdminError {
    kind: SystemAdminErrorKind,
    message: String,
}

impl SystemAdminError {
    #[must_use]
    pub fn new(kind: SystemAdminErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> SystemAdminErrorKind {
        self.kind
    }
}

/// 系统版本、更新与进程控制应用端口。
#[async_trait]
pub trait SystemAdminService: Send + Sync {
    async fn version(&self) -> Result<Value, SystemAdminError>;
    async fn update_detail(&self, refresh: bool) -> Result<Value, SystemAdminError>;
    fn update_events(&self) -> SystemUpdateEventStream;
    async fn perform_update(
        &self,
        target_version: Option<String>,
    ) -> Result<Value, SystemAdminError>;
    async fn update_status(&self) -> Result<Value, SystemAdminError>;
    async fn rollback(&self) -> Result<Value, SystemAdminError>;
    async fn restart(&self) -> Result<Value, SystemAdminError>;
}

/// 系统管理 HTTP module 所需最小 state。
pub trait SystemAdminState: AdminSessionState {
    fn system_admin_service(&self) -> &dyn SystemAdminService;
}

/// 构造固定 GET/POST 系统管理路由。
pub fn router<S>() -> Router<S>
where
    S: SystemAdminState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/system/version", get(version::<S>))
        .route("/api/admin/system/update-detail", get(update_detail::<S>))
        .route(
            "/api/admin/system/update-events",
            get(update_event_stream::<S>),
        )
        .route("/api/admin/system/update", post(perform_update::<S>))
        .route("/api/admin/system/update-status", get(update_status::<S>))
        .route("/api/admin/system/rollback", post(rollback::<S>))
        .route("/api/admin/system/restart", post(restart::<S>))
}

async fn version<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SystemAdminState + Send + Sync,
{
    value_response(state.system_admin_service().version().await)
}

async fn update_detail<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<UpdateDetailQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SystemAdminState + Send + Sync,
{
    value_response(
        state
            .system_admin_service()
            .update_detail(query.refresh())
            .await,
    )
}

async fn update_event_stream<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AdminError>
where
    S: SystemAdminState + Send + Sync,
{
    let stream = state.system_admin_service().update_events().map(|message| {
        let data = serde_json::to_string(&message.data).unwrap_or_else(|_| "{}".to_owned());
        Ok(Event::default().event("update").id(message.id).data(data))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn perform_update<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    payload: Option<Json<UpdateRequest>>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SystemAdminState + Send + Sync,
{
    value_response(
        state
            .system_admin_service()
            .perform_update(payload.map(|Json(value)| value.into_target_version()))
            .await,
    )
}

async fn update_status<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SystemAdminState + Send + Sync,
{
    value_response(state.system_admin_service().update_status().await)
}

async fn rollback<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SystemAdminState + Send + Sync,
{
    value_response(state.system_admin_service().rollback().await)
}

async fn restart<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SystemAdminState + Send + Sync,
{
    value_response(state.system_admin_service().restart().await)
}

fn value_response(
    result: Result<Value, SystemAdminError>,
) -> Result<AdminResponse<AdminEnvelope<Value>>, AdminError> {
    let data = result.map_err(map_system_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

fn map_system_error(error: SystemAdminError) -> AdminError {
    match error.kind() {
        SystemAdminErrorKind::Invalid => AdminError::bad_request(error.to_string()),
        SystemAdminErrorKind::Conflict => AdminError::conflict(error.to_string()),
        SystemAdminErrorKind::BadGateway => AdminError::bad_gateway(error.to_string()),
        SystemAdminErrorKind::Internal => AdminError::internal(error.to_string()),
    }
}
