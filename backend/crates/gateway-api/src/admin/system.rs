//! 系统管理接口的查询与请求 wire contract。
//!
//! 这里不依赖更新服务或进程控制；应用层通过窄端口提供系统操作事实。

use std::convert::Infallible;

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
use gateway_admin::model::system::{
    SystemOperationAccepted, SystemOperationKind, SystemOperationState, SystemOperationStatus,
    SystemUpdateDetail, SystemUpdateEvent, SystemUpdateEventLevel, SystemUpdateStatus,
    SystemVersion,
};
use serde::{Deserialize, Serialize};

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminResponse, AdminSessionState,
    wire::map_admin_service_error,
};

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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemVersionView {
    version: String,
    git_sha: String,
    build_time: String,
    deployment_mode: String,
    deployment_mode_label: String,
    update_channel: String,
    latest_version: String,
    has_update: bool,
    update_cached: bool,
    update_warning: Option<String>,
}

impl From<SystemVersion> for SystemVersionView {
    fn from(version: SystemVersion) -> Self {
        Self {
            deployment_mode_label: deployment_mode_label(&version.deployment_mode).to_owned(),
            version: version.version,
            git_sha: version.git_sha,
            build_time: version.build_time,
            deployment_mode: version.deployment_mode,
            update_channel: version.update_channel,
            latest_version: version.latest_version,
            has_update: version.has_update,
            update_cached: version.update_cached,
            update_warning: version.update_warning,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemUpdateDetailView {
    current_version: String,
    latest_version: String,
    has_update: bool,
    deployment_mode: String,
    deployment_mode_label: String,
    build_type: String,
    build_type_label: String,
    release_url: Option<String>,
    notes: Option<String>,
    cached: bool,
    update_supported: bool,
    unsupported_reason: Option<String>,
    warning: Option<String>,
}

impl From<SystemUpdateDetail> for SystemUpdateDetailView {
    fn from(detail: SystemUpdateDetail) -> Self {
        Self {
            deployment_mode_label: deployment_mode_label(&detail.deployment_mode).to_owned(),
            build_type_label: build_type_label(&detail.build_type),
            current_version: detail.current_version,
            latest_version: detail.latest_version,
            has_update: detail.has_update,
            deployment_mode: detail.deployment_mode,
            build_type: detail.build_type,
            release_url: detail.release_url,
            notes: detail.notes,
            cached: detail.cached,
            update_supported: detail.update_supported,
            unsupported_reason: detail.unsupported_reason,
            warning: detail.warning,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemOperationStateView {
    operation_id: Option<String>,
    kind: Option<&'static str>,
    status: &'static str,
    target_version: Option<String>,
    message: Option<String>,
    error: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
}

impl From<SystemOperationState> for SystemOperationStateView {
    fn from(operation: SystemOperationState) -> Self {
        Self {
            operation_id: operation.operation_id,
            kind: operation.kind.map(operation_kind_name),
            status: operation_status_name(operation.status),
            target_version: operation.target_version,
            message: operation.message,
            error: operation.error,
            started_at: operation.started_at.map(|value| value.to_rfc3339()),
            finished_at: operation.finished_at.map(|value| value.to_rfc3339()),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemUpdateStatusView {
    previous_version: Option<String>,
    current_version: Option<String>,
    operation: SystemOperationStateView,
}

impl From<SystemUpdateStatus> for SystemUpdateStatusView {
    fn from(status: SystemUpdateStatus) -> Self {
        Self {
            previous_version: status.previous_version,
            current_version: status.current_version,
            operation: status.operation.into(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateAcceptedView {
    operation_id: String,
    deployment_mode: String,
    message: String,
    need_restart: bool,
    target_version: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RollbackAcceptedView {
    message: String,
    need_restart: bool,
    operation_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RestartAcceptedView {
    message: String,
    operation_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemUpdateEventView {
    operation_id: Option<String>,
    level: &'static str,
    step: Option<String>,
    message: String,
    terminal: bool,
    at: String,
}

/// 构造固定 GET/POST 系统管理路由。
pub fn router<S>() -> Router<S>
where
    S: AdminSessionState + Clone + Send + Sync + 'static,
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
    S: AdminSessionState + Send + Sync,
{
    let version = state
        .admin_services()
        .system()
        .version()
        .await
        .map_err(map_system_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(SystemVersionView::from(version)),
    ))
}

async fn update_detail<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<UpdateDetailQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let detail = state
        .admin_services()
        .system()
        .update_detail(query.refresh())
        .await
        .map_err(map_system_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(SystemUpdateDetailView::from(detail)),
    ))
}

async fn update_event_stream<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let stream = state
        .admin_services()
        .system()
        .update_events()
        .map(|message| {
            let id = message.id.clone();
            let data = SystemUpdateEventView::from(message).into_json().to_string();
            Ok(Event::default().event("update").id(id).data(data))
        });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn perform_update<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    payload: Option<Json<UpdateRequest>>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .system()
        .perform_update(payload.map(|Json(value)| value.into_target_version()))
        .await
        .map_err(map_system_error)?;
    let SystemOperationAccepted::Update {
        operation_id,
        deployment_mode,
        message,
        need_restart,
        target_version,
    } = result
    else {
        return Err(AdminError::internal("Invalid system update result"));
    };
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(UpdateAcceptedView {
            operation_id,
            deployment_mode,
            message,
            need_restart,
            target_version,
        }),
    ))
}

async fn update_status<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let status = state
        .admin_services()
        .system()
        .update_status()
        .await
        .map_err(map_system_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(SystemUpdateStatusView::from(status)),
    ))
}

async fn rollback<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .system()
        .rollback()
        .await
        .map_err(map_system_error)?;
    let SystemOperationAccepted::Rollback {
        operation_id,
        message,
        need_restart,
    } = result
    else {
        return Err(AdminError::internal("Invalid system rollback result"));
    };
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(RollbackAcceptedView {
            message,
            need_restart,
            operation_id,
        }),
    ))
}

async fn restart<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .system()
        .restart()
        .await
        .map_err(map_system_error)?;
    let SystemOperationAccepted::Restart {
        operation_id,
        message,
    } = result
    else {
        return Err(AdminError::internal("Invalid system restart result"));
    };
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(RestartAcceptedView {
            message,
            operation_id,
        }),
    ))
}

impl From<SystemUpdateEvent> for SystemUpdateEventView {
    fn from(event: SystemUpdateEvent) -> Self {
        Self {
            operation_id: event.operation_id,
            level: update_event_level_name(event.level),
            step: event.step,
            message: event.message,
            terminal: event.terminal,
            at: event.occurred_at.to_rfc3339(),
        }
    }
}

impl SystemUpdateEventView {
    fn into_json(self) -> serde_json::Value {
        serde_json::json!({
            "operationId": self.operation_id,
            "level": self.level,
            "step": self.step,
            "message": self.message,
            "terminal": self.terminal,
            "at": self.at,
        })
    }
}

fn deployment_mode_label(mode: &str) -> &'static str {
    match mode {
        "docker" => "Docker",
        "binary" => "二进制",
        _ => "源码运行",
    }
}

fn build_type_label(build_type: &str) -> String {
    match build_type {
        "release" => "正式构建",
        "source" => "源码构建",
        "dev" => "开发构建",
        value => value,
    }
    .to_owned()
}

const fn operation_kind_name(kind: SystemOperationKind) -> &'static str {
    match kind {
        SystemOperationKind::Update => "update",
        SystemOperationKind::Rollback => "rollback",
        SystemOperationKind::Restart => "restart",
    }
}

const fn operation_status_name(status: SystemOperationStatus) -> &'static str {
    match status {
        SystemOperationStatus::Idle => "idle",
        SystemOperationStatus::Running => "running",
        SystemOperationStatus::Succeeded => "succeeded",
        SystemOperationStatus::Failed => "failed",
    }
}

const fn update_event_level_name(level: SystemUpdateEventLevel) -> &'static str {
    match level {
        SystemUpdateEventLevel::Info => "info",
        SystemUpdateEventLevel::Warning => "warning",
        SystemUpdateEventLevel::Success => "success",
        SystemUpdateEventLevel::Error => "error",
    }
}

fn map_system_error(error: gateway_admin::model::AdminError) -> AdminError {
    map_admin_service_error(error, "System operation unavailable")
}
