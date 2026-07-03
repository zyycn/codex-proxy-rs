//! 管理端系统版本与在线更新路由。

use std::{
    convert::Infallible,
    env, fmt, fs, io,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

use axum::{
    extract::Query,
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
};
use chrono::Utc;
use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex};

use crate::{
    admin::auth::session::AdminAuth,
    admin::response::{AdminEnvelope, AdminError, AdminResponse},
    runtime::shutdown::request_shutdown,
};

use super::state::{
    finish_operation, operation_id, read_state, set_operation_running, OperationLock,
    SystemOperationKind,
};

mod download;
use download::{
    download_file, validate_download_url, validate_github_api_base, verify_checksum,
    DownloadProgress,
};
mod archive;
use archive::{extract_release_archive, replace_release_files, rollback_release_update};
mod release;
use release::{
    check_latest_release, fetch_latest_release, select_release_archive, update_info_from_release,
    GitHubRelease,
};

const APP_BINARY_NAME: &str = "codex-proxy-rs";
const DEFAULT_WEB_DIST_DIR: &str = "/app/web/dist";
const GITHUB_API_BASE: &str = "https://api.github.com/repos";
const MAX_DOWNLOAD_SIZE: u64 = 500 * 1024 * 1024;

static SYSTEM_OPERATION_LOCK: Mutex<()> = Mutex::const_new(());
static UPDATE_EVENT_SENDER: OnceLock<broadcast::Sender<SystemUpdateEvent>> = OnceLock::new();

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckUpdateQuery {
    force: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionData {
    version: String,
    git_sha: String,
    build_time: String,
    deployment_mode: String,
    deployment_mode_label: String,
    update_channel: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateStartedData {
    operation_id: String,
    deployment_mode: String,
    message: String,
    need_restart: bool,
    target_version: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum UpdateLogLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemUpdateEvent {
    id: String,
    operation_id: Option<String>,
    level: UpdateLogLevel,
    step: Option<String>,
    message: String,
    at: String,
}

#[derive(Debug, Clone)]
struct SystemUpdateConfig {
    version: String,
    git_sha: String,
    build_time: String,
    deployment_mode: String,
    build_type: String,
    update_channel: String,
    update_repository: Option<String>,
    github_api_base: String,
    executable_path: Option<PathBuf>,
    web_dist_dir: PathBuf,
    update_state_file: PathBuf,
    update_lock_file: PathBuf,
    update_temp_dir: PathBuf,
}

/// `GET /api/admin/system/version`
pub(crate) async fn version(_auth: AdminAuth) -> Result<impl IntoResponse, AdminError> {
    let config = SystemUpdateConfig::from_env();
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(config.version_data()),
    ))
}

/// `GET /api/admin/system/check-updates`
pub(crate) async fn check_updates(
    _auth: AdminAuth,
    Query(query): Query<CheckUpdateQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let config = SystemUpdateConfig::from_env();
    let info = check_latest_release(&config, query.force.unwrap_or(false)).await;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(info)))
}

/// `GET /api/admin/system/update-events`
pub(crate) async fn update_event_stream(
    _auth: AdminAuth,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AdminError> {
    let receiver = update_event_sender().subscribe();
    let stream = futures::stream::unfold(receiver, |mut receiver| async move {
        loop {
            match receiver.recv().await {
                Ok(message) => {
                    let id = message.id.clone();
                    let data = serde_json::to_string(&message).unwrap_or_else(|_| "{}".to_string());
                    let event = Event::default().event("update").id(id).data(data);
                    return Some((Ok(event), receiver));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// `POST /api/admin/system/update`
pub(crate) async fn perform_update(_auth: AdminAuth) -> Result<impl IntoResponse, AdminError> {
    let _guard = SYSTEM_OPERATION_LOCK
        .try_lock()
        .map_err(|_| AdminError::conflict("System update already running"))?;
    let config = SystemUpdateConfig::from_env();
    if let Some(reason) = config.update_support_error() {
        emit_update_event(
            UpdateLogLevel::Error,
            None,
            Some("preflight"),
            reason.clone(),
        );
        return Err(AdminError::conflict(reason));
    }

    emit_update_event(
        UpdateLogLevel::Info,
        None,
        Some("release"),
        "正在获取最新 Release 信息",
    );
    let release = fetch_latest_release(
        &config.github_api_base,
        config
            .update_repository
            .as_deref()
            .ok_or_else(|| AdminError::conflict("Update checks require CPR_UPDATE_REPOSITORY"))?,
    )
    .await
    .map_err(|error| {
        emit_update_event(UpdateLogLevel::Error, None, Some("release"), error.clone());
        AdminError::bad_gateway(error)
    })?;
    let info = update_info_from_release(&config, release.clone());
    if !info.has_update {
        emit_update_event(
            UpdateLogLevel::Warning,
            None,
            Some("release"),
            "当前版本已是最新",
        );
        return Err(AdminError::conflict("Already up to date"));
    }
    let target_version = info.latest_version.clone();
    let operation_id = operation_id("update");
    let lock = OperationLock::acquire(&config.update_lock_file)?;
    set_operation_running(
        &config.update_state_file,
        &operation_id,
        SystemOperationKind::Update,
        Some(&target_version),
    )?;

    emit_update_event(
        UpdateLogLevel::Info,
        Some(&operation_id),
        Some("prepare"),
        format!("准备更新到 v{target_version}"),
    );
    let result = perform_release_update(&config, &release, &target_version, &operation_id).await;
    if let Err(error) = &result {
        emit_update_event(
            UpdateLogLevel::Error,
            Some(&operation_id),
            Some("failed"),
            error.to_string(),
        );
    } else {
        emit_update_event(
            UpdateLogLevel::Success,
            Some(&operation_id),
            Some("done"),
            "更新文件已替换，等待服务重启生效",
        );
    }
    finish_operation(
        &config.update_state_file,
        &operation_id,
        SystemOperationKind::Update,
        result.as_ref().ok().map(|_| target_version.clone()),
        result.as_ref().err().map(ToString::to_string),
    );
    drop(lock);
    result?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(UpdateStartedData {
            operation_id,
            deployment_mode: config.deployment_mode,
            message: "更新完成，请重启服务。".to_string(),
            need_restart: true,
            target_version,
        }),
    ))
}

/// `GET /api/admin/system/update-status`
pub(crate) async fn update_status(_auth: AdminAuth) -> Result<impl IntoResponse, AdminError> {
    let config = SystemUpdateConfig::from_env();
    let status = read_state(&config.update_state_file)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(status),
    ))
}

/// `POST /api/admin/system/rollback`
pub(crate) async fn rollback(_auth: AdminAuth) -> Result<impl IntoResponse, AdminError> {
    let _guard = SYSTEM_OPERATION_LOCK
        .try_lock()
        .map_err(|_| AdminError::conflict("系统操作正在执行中"))?;
    let config = SystemUpdateConfig::from_env();
    if let Some(reason) = config.update_support_error() {
        return Err(AdminError::conflict(reason));
    }

    let operation_id = operation_id("rollback");
    let lock = OperationLock::acquire(&config.update_lock_file)?;
    set_operation_running(
        &config.update_state_file,
        &operation_id,
        SystemOperationKind::Rollback,
        None,
    )?;
    let result = rollback_release_update(&config).await;
    finish_operation(
        &config.update_state_file,
        &operation_id,
        SystemOperationKind::Rollback,
        None,
        result.as_ref().err().map(ToString::to_string),
    );
    drop(lock);
    result?;

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({
            "message": "回滚完成，请重启服务。",
            "needRestart": true,
            "operationId": operation_id
        })),
    ))
}

/// `POST /api/admin/system/restart`
pub(crate) async fn restart(_auth: AdminAuth) -> Result<impl IntoResponse, AdminError> {
    if env_string("CPR_ENABLE_SELF_RESTART").as_deref() != Some("true") {
        return Err(AdminError::conflict(
            "自重启未启用，请设置 CPR_ENABLE_SELF_RESTART=true",
        ));
    }

    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        request_shutdown();
    });

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(serde_json::json!({
            "message": "已安排重启",
            "operationId": operation_id("restart")
        })),
    ))
}

impl SystemUpdateConfig {
    fn from_env() -> Self {
        let update_repository = env_string("CPR_UPDATE_REPOSITORY");
        let state_file = env_string("CPR_UPDATE_STATE_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/app/data/update-state.json"));
        let update_lock_file = env_string("CPR_UPDATE_LOCK_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| state_file.with_extension("lock"));
        let update_temp_dir = env_string("CPR_UPDATE_TEMP_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| default_update_temp_dir(&state_file));
        Self {
            version: build_version(),
            git_sha: build_git_sha(),
            build_time: build_time(),
            deployment_mode: env_string("CPR_DEPLOYMENT_MODE")
                .unwrap_or_else(|| "source".to_string()),
            build_type: build_type(),
            update_channel: env_string("CPR_UPDATE_CHANNEL")
                .unwrap_or_else(|| "stable".to_string()),
            update_repository,
            github_api_base: env_string("CPR_GITHUB_API_BASE")
                .unwrap_or_else(|| GITHUB_API_BASE.to_string()),
            executable_path: env_string("CPR_UPDATE_EXE_PATH").map(PathBuf::from),
            web_dist_dir: env_string("CPR_WEB_DIST_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_WEB_DIST_DIR)),
            update_state_file: state_file,
            update_lock_file,
            update_temp_dir,
        }
    }

    fn version_data(&self) -> VersionData {
        VersionData {
            version: self.version.clone(),
            git_sha: self.git_sha.clone(),
            build_time: self.build_time.clone(),
            deployment_mode: self.deployment_mode.clone(),
            deployment_mode_label: deployment_mode_label(&self.deployment_mode).to_string(),
            update_channel: self.update_channel.clone(),
        }
    }

    fn update_support_error(&self) -> Option<String> {
        if self.build_type != "release" {
            return Some("一键更新需要正式构建包".to_string());
        }
        if self.update_repository.is_none() {
            return Some("检查更新需要配置 CPR_UPDATE_REPOSITORY".to_string());
        }
        if let Err(error) = validate_github_api_base(&self.github_api_base) {
            return Some(error);
        }
        None
    }

    fn release_cache_key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.update_repository.as_deref().unwrap_or_default(),
            self.version,
            self.deployment_mode,
            self.build_type,
            self.update_channel,
        )
    }

    fn executable_path(&self) -> Result<PathBuf, AdminError> {
        if let Some(path) = &self.executable_path {
            return Ok(path.clone());
        }
        env::current_exe()
            .and_then(fs::canonicalize)
            .map_err(internal_error_with("Failed to resolve executable"))
    }
}

fn internal_error(context: &str, error: impl fmt::Display) -> AdminError {
    AdminError::internal(format!("{context}: {error}"))
}

fn internal_error_with<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> AdminError {
    move |error| internal_error(context, error)
}

fn bad_request_with<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> AdminError {
    move |error| AdminError::bad_request(format!("{context}: {error}"))
}

fn bad_gateway_with<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> AdminError {
    move |error| AdminError::bad_gateway(format!("{context}: {error}"))
}

async fn perform_release_update(
    config: &SystemUpdateConfig,
    release: &GitHubRelease,
    version: &str,
    operation_id: &str,
) -> Result<(), AdminError> {
    emit_update_event(
        UpdateLogLevel::Info,
        Some(operation_id),
        Some("asset"),
        "正在选择匹配当前平台的更新包",
    );
    let archive = select_release_archive(release, version)?;
    emit_update_event(
        UpdateLogLevel::Info,
        Some(operation_id),
        Some("asset"),
        format!(
            "已选择更新包 {} ({})",
            archive.name,
            format_bytes(archive.size)
        ),
    );

    emit_update_event(
        UpdateLogLevel::Info,
        Some(operation_id),
        Some("verify"),
        "正在校验下载地址",
    );
    validate_download_url(&archive.browser_download_url, &config.github_api_base)?;
    if archive.size > MAX_DOWNLOAD_SIZE {
        return Err(AdminError::bad_request("Release archive is too large"));
    }
    let checksum = release
        .assets
        .iter()
        .find(|asset| asset.name == "checksums.txt")
        .ok_or_else(|| AdminError::bad_gateway("Release checksums.txt is required"))?;
    validate_download_url(&checksum.browser_download_url, &config.github_api_base)?;

    emit_update_event(
        UpdateLogLevel::Info,
        Some(operation_id),
        Some("prepare"),
        "正在创建临时更新目录",
    );
    fs::create_dir_all(&config.update_temp_dir)
        .map_err(internal_error_with("Failed to prepare update temp dir"))?;
    let temp_dir = fs::canonicalize(&config.update_temp_dir)
        .and_then(|dir| tempfile_dir_in(&dir, ".codex-proxy-rs-update-"))
        .map_err(internal_error_with("Failed to create update temp dir"))?;
    let archive_path = temp_dir.join(&archive.name);

    emit_update_event(
        UpdateLogLevel::Info,
        Some(operation_id),
        Some("download"),
        "开始下载更新包",
    );
    download_file(
        &archive.browser_download_url,
        &archive_path,
        MAX_DOWNLOAD_SIZE,
        Some(DownloadProgress {
            operation_id,
            total_size: archive.size,
        }),
    )
    .await?;
    emit_update_event(
        UpdateLogLevel::Success,
        Some(operation_id),
        Some("download"),
        "更新包下载完成",
    );

    emit_update_event(
        UpdateLogLevel::Info,
        Some(operation_id),
        Some("checksum"),
        "正在校验 checksum",
    );
    verify_checksum(&archive_path, &archive.name, &checksum.browser_download_url).await?;
    emit_update_event(
        UpdateLogLevel::Success,
        Some(operation_id),
        Some("checksum"),
        "checksum 校验通过",
    );

    emit_update_event(
        UpdateLogLevel::Info,
        Some(operation_id),
        Some("extract"),
        "正在解压更新包",
    );
    let extracted = extract_release_archive(&archive_path, &temp_dir)?;
    emit_update_event(
        UpdateLogLevel::Success,
        Some(operation_id),
        Some("extract"),
        "更新包解压完成",
    );

    emit_update_event(
        UpdateLogLevel::Info,
        Some(operation_id),
        Some("replace"),
        "正在替换应用文件",
    );
    let exe_path = config.executable_path()?;
    replace_release_files(&exe_path, &config.web_dist_dir, extracted)?;
    emit_update_event(
        UpdateLogLevel::Success,
        Some(operation_id),
        Some("replace"),
        "应用文件替换完成",
    );
    let _ = fs::remove_dir_all(temp_dir);
    Ok(())
}

fn tempfile_dir_in(parent: &Path, prefix: &str) -> io::Result<PathBuf> {
    for attempt in 0..100 {
        let path = parent.join(format!(
            "{prefix}{}-{attempt}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "failed to create unique temp dir",
    ))
}

fn default_update_temp_dir(state_file: &Path) -> PathBuf {
    state_file
        .parent()
        .map(|parent| parent.join("update-tmp"))
        .unwrap_or_else(|| env::temp_dir().join("codex-proxy-rs-update"))
}

fn env_string(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn runtime_build_info_override(key: &str) -> Option<String> {
    if cfg!(debug_assertions) {
        env_string(key)
    } else {
        None
    }
}

fn build_version() -> String {
    runtime_build_info_override("CPR_VERSION").unwrap_or_else(|| {
        option_env!("CPR_VERSION")
            .unwrap_or(env!("CARGO_PKG_VERSION"))
            .to_string()
    })
}

fn build_git_sha() -> String {
    runtime_build_info_override("CPR_GIT_SHA")
        .unwrap_or_else(|| option_env!("CPR_GIT_SHA").unwrap_or("unknown").to_string())
}

fn build_time() -> String {
    runtime_build_info_override("CPR_BUILD_TIME").unwrap_or_else(|| {
        option_env!("CPR_BUILD_TIME")
            .unwrap_or("unknown")
            .to_string()
    })
}

fn build_type() -> String {
    runtime_build_info_override("CPR_BUILD_TYPE").unwrap_or_else(|| {
        option_env!("CPR_BUILD_TYPE")
            .unwrap_or("source")
            .to_string()
    })
}

fn update_event_sender() -> &'static broadcast::Sender<SystemUpdateEvent> {
    UPDATE_EVENT_SENDER.get_or_init(|| {
        let (sender, _receiver) = broadcast::channel(256);
        sender
    })
}

fn emit_update_event(
    level: UpdateLogLevel,
    operation_id: Option<&str>,
    step: Option<&str>,
    message: impl Into<String>,
) {
    let now = Utc::now();
    let event = SystemUpdateEvent {
        id: format!(
            "update-log-{}",
            now.timestamp_nanos_opt()
                .unwrap_or_else(|| now.timestamp_millis())
        ),
        operation_id: operation_id.map(ToString::to_string),
        level,
        step: step.map(ToString::to_string),
        message: message.into(),
        at: now.to_rfc3339(),
    };
    let _ = update_event_sender().send(event);
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    if bytes >= MIB {
        return format!("{:.1} MiB", bytes as f64 / MIB as f64);
    }
    if bytes >= KIB {
        return format!("{:.1} KiB", bytes as f64 / KIB as f64);
    }
    format!("{bytes} B")
}

fn deployment_mode_label(value: &str) -> &str {
    match value {
        "docker" => "Docker",
        "source" => "源码部署",
        "binary" => "二进制部署",
        _ => value,
    }
}

fn build_type_label(value: &str) -> &str {
    match value {
        "release" => "正式构建",
        "source" => "源码构建",
        "dev" => "开发构建",
        _ => value,
    }
}
