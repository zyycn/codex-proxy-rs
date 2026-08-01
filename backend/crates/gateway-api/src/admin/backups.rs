//! 备份设置、计划、手动创建、记录、下载与删除路由。
//!
//! 路径统一位于 `/api/admin/settings/backups/*`，内部由独立 `BackupService` 承担业务。

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use gateway_admin::model::backup::{
    BackupRecord, BackupSettings, BackupStatus, BackupTriggerKind, ConnectionTestResult,
    UpdateBackupScheduleCommand, UpdateBackupStorageCommand,
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminPageData, AdminResponse, AdminSessionState,
    PageMeta, wire::map_admin_service_error,
};

/// 备份设置视图；`secretAccessKey` 返回已保存的明文凭据，由前端掩码显示。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupSettingsView {
    pub storage_revision: u64,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub bucket: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    pub prefix: Option<String>,
    pub force_path_style: bool,
    pub verified: bool,
    pub schedule_enabled: bool,
    pub cron_expression: Option<String>,
    pub schedule_timezone: Option<String>,
    pub retention_days: u32,
    pub retention_count: u32,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

impl From<BackupSettings> for BackupSettingsView {
    fn from(settings: BackupSettings) -> Self {
        let verified = settings.storage_verified();
        Self {
            storage_revision: settings.storage_revision,
            endpoint: settings.endpoint,
            region: settings.region,
            bucket: settings.bucket,
            access_key_id: settings.access_key_id,
            secret_access_key: settings
                .secret_access_key
                .map(|secret| secret.expose_secret().to_owned()),
            prefix: settings.prefix,
            force_path_style: settings.force_path_style,
            verified,
            schedule_enabled: settings.schedule_enabled,
            cron_expression: settings.cron_expression,
            schedule_timezone: settings.schedule_timezone,
            retention_days: settings.retention_days,
            retention_count: settings.retention_count,
            next_run_at: settings.next_run_at,
            last_verified_at: settings.last_verified_at,
            updated_at: settings.updated_at,
        }
    }
}

/// 更新 S3 存储配置请求；`secretAccessKey` 缺省表示保留旧值。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateBackupStorageRequest {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    #[serde(default)]
    pub secret_access_key: Option<String>,
    pub prefix: String,
    pub force_path_style: bool,
}

/// 更新调度配置请求。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateBackupScheduleRequest {
    pub schedule_enabled: bool,
    pub cron_expression: String,
    pub schedule_timezone: String,
    pub retention_days: u32,
    pub retention_count: u32,
}

/// 备份记录 wire 视图。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupRecordView {
    pub id: String,
    pub trigger_kind: String,
    pub status: String,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub object_key: String,
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
    pub attempt_count: u32,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<BackupRecord> for BackupRecordView {
    fn from(record: BackupRecord) -> Self {
        Self {
            id: record.id,
            trigger_kind: record.trigger_kind.to_string(),
            status: record.status.to_string(),
            scheduled_at: record.scheduled_at,
            object_key: record.object_key,
            size_bytes: record.size_bytes,
            sha256: record.sha256,
            attempt_count: record.attempt_count,
            error_code: record.error_code,
            error_message: record.error_message,
            started_at: record.started_at,
            completed_at: record.completed_at,
            expires_at: record.expires_at,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

/// 记录列表查询参数。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupRecordsQuery {
    pub page: Option<u32>,
    pub page_size: Option<u16>,
    pub status: Option<String>,
    pub trigger: Option<String>,
}

/// 下载地址请求。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupIdRequest {
    pub backup_id: String,
}

/// 下载地址响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadUrlView {
    pub url: String,
    pub file_name: String,
    pub expires_in_seconds: u64,
}

impl From<gateway_admin::model::backup::DownloadUrlResult> for DownloadUrlView {
    fn from(result: gateway_admin::model::backup::DownloadUrlResult) -> Self {
        Self {
            url: result.url,
            file_name: result.file_name,
            expires_in_seconds: result.expires_in.as_secs(),
        }
    }
}

/// 连接测试响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionTestView {
    pub ok: bool,
    pub stage: String,
    pub code: Option<String>,
    pub message: String,
}

impl From<ConnectionTestResult> for ConnectionTestView {
    fn from(result: ConnectionTestResult) -> Self {
        Self {
            ok: result.ok,
            stage: result.stage.to_owned(),
            code: result.code.map(str::to_owned),
            message: result.message,
        }
    }
}

/// 构造固定 GET/POST 备份路由。
pub fn router<S>() -> Router<S>
where
    S: AdminSessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/settings/backups", get(backup_settings::<S>))
        .route(
            "/api/admin/settings/backups/storage/update",
            post(update_backup_storage::<S>),
        )
        .route(
            "/api/admin/settings/backups/storage/test",
            post(test_backup_storage::<S>),
        )
        .route(
            "/api/admin/settings/backups/schedule/update",
            post(update_backup_schedule::<S>),
        )
        .route(
            "/api/admin/settings/backups/records",
            get(backup_records::<S>),
        )
        .route(
            "/api/admin/settings/backups/create",
            post(create_backup::<S>),
        )
        .route(
            "/api/admin/settings/backups/download-url",
            post(download_backup_url::<S>),
        )
        .route(
            "/api/admin/settings/backups/delete",
            post(delete_backup::<S>),
        )
}

async fn backup_settings<S>(
    _auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let settings = state
        .admin_services()
        .backups()
        .load_settings()
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(BackupSettingsView::from(settings)),
    ))
}

async fn update_backup_storage<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<UpdateBackupStorageRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = UpdateBackupStorageCommand {
        endpoint: request.endpoint,
        region: request.region,
        bucket: request.bucket,
        access_key_id: request.access_key_id,
        secret_access_key: request.secret_access_key.map(SecretString::from),
        prefix: request.prefix,
        force_path_style: request.force_path_style,
    };
    let settings = state
        .admin_services()
        .backups()
        .update_storage(&auth.context().mutation_context(), command)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(BackupSettingsView::from(settings)),
    ))
}

async fn test_backup_storage<S>(
    auth: AdminAuth,
    State(state): State<S>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .backups()
        .test_connection(&auth.context().mutation_context())
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(ConnectionTestView::from(result)),
    ))
}

async fn update_backup_schedule<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<UpdateBackupScheduleRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let command = UpdateBackupScheduleCommand {
        schedule_enabled: request.schedule_enabled,
        cron_expression: request.cron_expression,
        schedule_timezone: request.schedule_timezone,
        retention_days: request.retention_days,
        retention_count: request.retention_count,
    };
    let settings = state
        .admin_services()
        .backups()
        .update_schedule(&auth.context().mutation_context(), command)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(BackupSettingsView::from(settings)),
    ))
}

async fn backup_records<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<BackupRecordsQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let page_size = gateway_admin::model::PageSize::new(query.page_size.unwrap_or(20))
        .map_err(|_| AdminError::bad_request("Invalid pageSize"))?;
    let page = query.page.unwrap_or(1);
    if page == 0 {
        return Err(AdminError::bad_request("Invalid page"));
    }
    let status = match query.status.as_deref() {
        None => None,
        Some(value) => BackupStatus::parse(value)
            .map(Some)
            .ok_or_else(|| AdminError::bad_request("Invalid status"))?,
    };
    let trigger = match query.trigger.as_deref() {
        None => None,
        Some(value) => BackupTriggerKind::parse(value)
            .map(Some)
            .ok_or_else(|| AdminError::bad_request("Invalid trigger"))?,
    };
    let domain_query = gateway_admin::model::backup::BackupRecordListQuery {
        page,
        page_size,
        status,
        trigger,
    };
    let page_result = state
        .admin_services()
        .backups()
        .list_records(domain_query)
        .await
        .map_err(map_service_error)?;
    let meta = PageMeta::new(
        page_result.page,
        u32::from(page_result.page_size.get()),
        page_result.total,
        page_result.total_pages(),
    );
    let items: Vec<BackupRecordView> = page_result
        .items
        .into_iter()
        .map(BackupRecordView::from)
        .collect();
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(AdminPageData::new(items, meta)),
    ))
}

async fn create_backup<S>(
    auth: AdminAuth,
    State(state): State<S>,
    body: Option<Json<CreateBackupRequest>>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let expires_at = body
        .and_then(|body| body.0.expires_in_days)
        .filter(|&days| days > 0)
        .map(|days| Utc::now() + chrono::Duration::days(i64::from(days)));
    let record = state
        .admin_services()
        .backups()
        .create_backup(&auth.context().mutation_context(), expires_at)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::ACCEPTED,
        AdminEnvelope::ok(BackupRecordView::from(record)),
    ))
}

/// 创建备份请求；`expiresInDays` 为 0 或缺省表示不设置过期时间。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateBackupRequest {
    pub expires_in_days: Option<u32>,
}

async fn download_backup_url<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<BackupIdRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .backups()
        .download_url(&auth.context().mutation_context(), &request.backup_id)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(DownloadUrlView::from(result)),
    ))
}

async fn delete_backup<S>(
    auth: AdminAuth,
    State(state): State<S>,
    Json(request): Json<BackupIdRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let record = state
        .admin_services()
        .backups()
        .delete_backup(&auth.context().mutation_context(), &request.backup_id)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(BackupRecordView::from(record)),
    ))
}

fn map_service_error(error: gateway_admin::model::AdminError) -> AdminError {
    map_admin_service_error(error, "Backup service unavailable")
}
