//! 管理端系统版本与在线更新路由。

use std::{
    convert::Infallible,
    env, fs, io,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::OnceLock,
    time::{Duration, Instant},
};

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
};
use chrono::Utc;
use flate2::read::GzDecoder;
use futures::Stream;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::Archive;
use tokio::sync::{broadcast, Mutex};

use crate::{
    admin::auth::session::require_admin_auth,
    admin::response::{AdminEnvelope, AdminError, AdminResponse},
    runtime::state::AppState,
};

const APP_BINARY_NAME: &str = "codex-proxy-rs";
const DEFAULT_WEB_DIST_DIR: &str = "/app/web/dist";
const GITHUB_API_BASE: &str = "https://api.github.com/repos";
const UPDATE_CACHE_TTL: Duration = Duration::from_secs(20 * 60);
const MAX_DOWNLOAD_SIZE: u64 = 500 * 1024 * 1024;

static SYSTEM_OPERATION_LOCK: Mutex<()> = Mutex::const_new(());
static RELEASE_CACHE: Mutex<Option<CachedUpdateInfo>> = Mutex::const_new(None);
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateInfoData {
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateStartedData {
    operation_id: String,
    deployment_mode: String,
    message: String,
    need_restart: bool,
    target_version: String,
}

#[derive(Debug, Clone)]
struct CachedUpdateInfo {
    key: String,
    info: UpdateInfoData,
    cached_at: Instant,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    html_url: Option<String>,
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
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

#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct SystemUpdateStatusData {
    previous_version: Option<String>,
    current_version: Option<String>,
    #[serde(default)]
    operation: SystemOperationState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum SystemOperationKind {
    Update,
    Rollback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
enum SystemOperationStatus {
    #[default]
    Idle,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemOperationState {
    operation_id: Option<String>,
    kind: Option<SystemOperationKind>,
    status: SystemOperationStatus,
    target_version: Option<String>,
    message: Option<String>,
    error: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
}

impl Default for SystemOperationState {
    fn default() -> Self {
        Self {
            operation_id: None,
            kind: None,
            status: SystemOperationStatus::Idle,
            target_version: None,
            message: None,
            error: None,
            started_at: None,
            finished_at: None,
        }
    }
}

#[derive(Debug)]
struct OperationLock {
    path: PathBuf,
}

#[derive(Debug)]
struct ExtractedRelease {
    binary_path: PathBuf,
    web_dist_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
struct DownloadProgress<'a> {
    operation_id: &'a str,
    total_size: u64,
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
}

/// `GET /api/admin/system/version`
pub(crate) async fn version(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    let config = SystemUpdateConfig::from_env();
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(config.version_data()),
    ))
}

/// `GET /api/admin/system/check-updates`
pub(crate) async fn check_updates(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CheckUpdateQuery>,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    let config = SystemUpdateConfig::from_env();
    let info = check_latest_release(&config, query.force.unwrap_or(false)).await;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(info)))
}

/// `GET /api/admin/system/update-events`
pub(crate) async fn update_event_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AdminError> {
    require_admin_auth(&state, &headers).await?;

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
pub(crate) async fn perform_update(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
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
pub(crate) async fn update_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    let config = SystemUpdateConfig::from_env();
    let status = read_state(&config.update_state_file)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(status),
    ))
}

/// `POST /api/admin/system/rollback`
pub(crate) async fn rollback(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
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
pub(crate) async fn restart(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    if env_string("CPR_ENABLE_SELF_RESTART").as_deref() != Some("true") {
        return Err(AdminError::conflict(
            "自重启未启用，请设置 CPR_ENABLE_SELF_RESTART=true",
        ));
    }

    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        std::process::exit(0);
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
            .map_err(|error| AdminError::internal(format!("Failed to resolve executable: {error}")))
    }
}

async fn check_latest_release(config: &SystemUpdateConfig, force: bool) -> UpdateInfoData {
    let Some(repository) = config.update_repository.as_deref() else {
        return UpdateInfoData {
            current_version: config.version.clone(),
            latest_version: config.version.clone(),
            has_update: false,
            deployment_mode: config.deployment_mode.clone(),
            deployment_mode_label: deployment_mode_label(&config.deployment_mode).to_string(),
            build_type: config.build_type.clone(),
            build_type_label: build_type_label(&config.build_type).to_string(),
            release_url: None,
            notes: None,
            cached: false,
            update_supported: false,
            unsupported_reason: Some("检查更新需要配置 CPR_UPDATE_REPOSITORY".to_string()),
            warning: None,
        };
    };
    let cache_key = config.release_cache_key();

    if !force {
        if let Some(info) = cached_release_info(&cache_key).await {
            return info;
        }
    }

    match fetch_latest_release(&config.github_api_base, repository).await {
        Ok(release) => {
            let info = update_info_from_release(config, release);
            cache_release_info(cache_key, &info).await;
            info
        }
        Err(error) => cached_release_info(&cache_key)
            .await
            .unwrap_or_else(|| UpdateInfoData {
                current_version: config.version.clone(),
                latest_version: config.version.clone(),
                has_update: false,
                deployment_mode: config.deployment_mode.clone(),
                deployment_mode_label: deployment_mode_label(&config.deployment_mode).to_string(),
                build_type: config.build_type.clone(),
                build_type_label: build_type_label(&config.build_type).to_string(),
                release_url: None,
                notes: None,
                cached: false,
                update_supported: false,
                unsupported_reason: config.update_support_error(),
                warning: Some(error),
            }),
    }
}

async fn cached_release_info(cache_key: &str) -> Option<UpdateInfoData> {
    let cache = RELEASE_CACHE.lock().await;
    let cached = cache.as_ref()?;
    if cached.key != cache_key || cached.cached_at.elapsed() > UPDATE_CACHE_TTL {
        return None;
    }
    let mut info = cached.info.clone();
    info.cached = true;
    Some(info)
}

async fn cache_release_info(cache_key: String, info: &UpdateInfoData) {
    let mut cache = RELEASE_CACHE.lock().await;
    *cache = Some(CachedUpdateInfo {
        key: cache_key,
        info: info.clone(),
        cached_at: Instant::now(),
    });
}

async fn fetch_latest_release(api_base: &str, repository: &str) -> Result<GitHubRelease, String> {
    let url = format!(
        "{}/{repository}/releases/latest",
        api_base.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(reqwest::header::USER_AGENT, APP_BINARY_NAME)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("GitHub release check failed with {status}"));
    }
    response
        .json::<GitHubRelease>()
        .await
        .map_err(|error| error.to_string())
}

fn update_info_from_release(config: &SystemUpdateConfig, release: GitHubRelease) -> UpdateInfoData {
    let latest_version = normalize_version_tag(&release.tag_name);
    let has_update = release_allowed(config, &release)
        && version_is_newer(&config.version, &latest_version).unwrap_or(false);
    let unsupported_reason = config.update_support_error();
    let update_supported = unsupported_reason.is_none();
    let notes = release.body.or(release.name);
    UpdateInfoData {
        current_version: config.version.clone(),
        latest_version,
        has_update,
        deployment_mode: config.deployment_mode.clone(),
        deployment_mode_label: deployment_mode_label(&config.deployment_mode).to_string(),
        build_type: config.build_type.clone(),
        build_type_label: build_type_label(&config.build_type).to_string(),
        release_url: release.html_url,
        notes,
        cached: false,
        update_supported,
        unsupported_reason,
        warning: None,
    }
}

fn release_allowed(config: &SystemUpdateConfig, release: &GitHubRelease) -> bool {
    config.update_channel != "stable" || !release.prerelease
}

fn version_is_newer(current: &str, latest: &str) -> Option<bool> {
    let current = Version::parse(&normalize_version_tag(current)).ok()?;
    let latest = Version::parse(&normalize_version_tag(latest)).ok()?;
    Some(latest > current)
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
    validate_download_url(&archive.browser_download_url)?;
    if archive.size > MAX_DOWNLOAD_SIZE {
        return Err(AdminError::bad_request("Release archive is too large"));
    }
    let checksum = release
        .assets
        .iter()
        .find(|asset| asset.name == "checksums.txt");
    if let Some(asset) = checksum {
        validate_download_url(&asset.browser_download_url)?;
    }

    emit_update_event(
        UpdateLogLevel::Info,
        Some(operation_id),
        Some("prepare"),
        "正在创建临时更新目录",
    );
    let exe_path = config.executable_path()?;
    let exe_dir = exe_path
        .parent()
        .ok_or_else(|| AdminError::internal("Executable path has no parent"))?;
    let temp_dir = fs::canonicalize(exe_dir)
        .and_then(|dir| fs::create_dir_all(&dir).map(|()| dir))
        .and_then(|dir| tempfile_dir_in(&dir, ".codex-proxy-rs-update-"))
        .map_err(|error| {
            AdminError::internal(format!("Failed to create update temp dir: {error}"))
        })?;
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

    if let Some(checksum) = checksum {
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
    }

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

async fn rollback_release_update(config: &SystemUpdateConfig) -> Result<(), AdminError> {
    let exe_path = config.executable_path()?;
    let backup_path = backup_path_for(&exe_path);
    if !backup_path.exists() {
        return Err(AdminError::conflict("No binary backup found for rollback"));
    }
    fs::rename(&backup_path, &exe_path)
        .map_err(|error| AdminError::internal(format!("Binary rollback failed: {error}")))?;

    let web_backup = backup_path_for(&config.web_dist_dir);
    if web_backup.exists() {
        if config.web_dist_dir.exists() {
            fs::remove_dir_all(&config.web_dist_dir).map_err(|error| {
                AdminError::internal(format!("Web rollback cleanup failed: {error}"))
            })?;
        }
        fs::rename(&web_backup, &config.web_dist_dir)
            .map_err(|error| AdminError::internal(format!("Web rollback failed: {error}")))?;
    }
    Ok(())
}

fn select_release_archive<'a>(
    release: &'a GitHubRelease,
    version: &str,
) -> Result<&'a GitHubAsset, AdminError> {
    let os = env::consts::OS;
    let arch = env::consts::ARCH;
    let normalized = normalize_version_tag(version);
    release
        .assets
        .iter()
        .find(|asset| {
            let name = asset.name.as_str();
            name.contains(APP_BINARY_NAME)
                && name.contains(&normalized)
                && name.contains(os)
                && name.contains(arch)
                && !name.ends_with(".txt")
        })
        .or_else(|| {
            release.assets.iter().find(|asset| {
                let name = asset.name.as_str();
                name.contains(os) && name.contains(arch) && !name.ends_with(".txt")
            })
        })
        .ok_or_else(|| {
            AdminError::conflict(format!(
                "No compatible release archive found for {os}/{arch}"
            ))
        })
}

async fn download_file(
    url: &str,
    dest: &Path,
    max_size: u64,
    progress: Option<DownloadProgress<'_>>,
) -> Result<(), AdminError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|error| AdminError::internal(format!("Failed to create HTTP client: {error}")))?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|error| AdminError::bad_gateway(format!("Download failed: {error}")))?;
    if !response.status().is_success() {
        return Err(AdminError::bad_gateway(format!(
            "Download failed with {}",
            response.status()
        )));
    }
    let mut file = fs::File::create(dest).map_err(|error| {
        AdminError::internal(format!("Failed to create download file: {error}"))
    })?;
    let mut downloaded = 0_u64;
    let mut next_progress = 10_u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| AdminError::bad_gateway(format!("Download stream failed: {error}")))?
    {
        downloaded += chunk.len() as u64;
        if downloaded > max_size {
            return Err(AdminError::bad_request("Download exceeds max allowed size"));
        }
        file.write_all(&chunk)
            .map_err(|error| AdminError::internal(format!("Failed to write download: {error}")))?;
        if let Some(progress) = progress {
            let percent = download_percent(downloaded, progress.total_size);
            if progress.total_size > 0
                && (percent >= next_progress || downloaded >= progress.total_size)
            {
                emit_update_event(
                    UpdateLogLevel::Info,
                    Some(progress.operation_id),
                    Some("download"),
                    format!(
                        "已下载 {} / {} ({percent}%)",
                        format_bytes(downloaded),
                        format_bytes(progress.total_size)
                    ),
                );
                next_progress = percent.saturating_add(10);
            }
        }
    }
    Ok(())
}

async fn verify_checksum(
    file_path: &Path,
    file_name: &str,
    checksum_url: &str,
) -> Result<(), AdminError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| AdminError::internal(format!("Failed to create HTTP client: {error}")))?;
    let body = client
        .get(checksum_url)
        .send()
        .await
        .map_err(|error| AdminError::bad_gateway(format!("Checksum download failed: {error}")))?
        .text()
        .await
        .map_err(|error| AdminError::bad_gateway(format!("Checksum read failed: {error}")))?;

    let expected = body.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?;
        (Path::new(name).file_name()?.to_string_lossy() == file_name).then(|| hash.to_string())
    });
    let expected = expected.ok_or_else(|| AdminError::bad_gateway("Checksum not found"))?;
    let actual = sha256_file(file_path)?;
    if expected.eq_ignore_ascii_case(&actual) {
        Ok(())
    } else {
        Err(AdminError::bad_gateway("Checksum mismatch"))
    }
}

fn sha256_file(path: &Path) -> Result<String, AdminError> {
    let mut file = fs::File::open(path)
        .map_err(|error| AdminError::internal(format!("Failed to open checksum file: {error}")))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            AdminError::internal(format!("Failed to read checksum file: {error}"))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn extract_release_archive(
    archive_path: &Path,
    temp_dir: &Path,
) -> Result<ExtractedRelease, AdminError> {
    let file = fs::File::open(archive_path)
        .map_err(|error| AdminError::internal(format!("Failed to open archive: {error}")))?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    let extract_dir = temp_dir.join("extracted");
    fs::create_dir_all(&extract_dir)
        .map_err(|error| AdminError::internal(format!("Failed to create extract dir: {error}")))?;
    let binary_path = temp_dir.join(APP_BINARY_NAME);
    let web_dist_dir = temp_dir.join("web-dist");
    let mut found_binary = false;
    let mut found_web = false;

    for entry in archive
        .entries()
        .map_err(|error| AdminError::internal(format!("Failed to read archive: {error}")))?
    {
        let mut entry = entry
            .map_err(|error| AdminError::internal(format!("Invalid archive entry: {error}")))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|error| AdminError::internal(format!("Invalid archive path: {error}")))?
            .to_path_buf();
        if unsafe_archive_path(&path) {
            return Err(AdminError::bad_request("Unsafe archive path"));
        }

        if path.file_name().is_some_and(|name| name == APP_BINARY_NAME) {
            entry.unpack(&binary_path).map_err(|error| {
                AdminError::internal(format!("Failed to extract binary: {error}"))
            })?;
            found_binary = true;
            continue;
        }

        if let Some(relative) = web_dist_relative_path(&path) {
            let target = web_dist_dir.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    AdminError::internal(format!("Failed to create web asset dir: {error}"))
                })?;
            }
            entry.unpack(&target).map_err(|error| {
                AdminError::internal(format!("Failed to extract web asset: {error}"))
            })?;
            found_web = true;
        }
    }

    if !found_binary {
        return Err(AdminError::bad_request(
            "Release archive does not contain codex-proxy-rs",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&binary_path, fs::Permissions::from_mode(0o755))
            .map_err(|error| AdminError::internal(format!("Failed to chmod binary: {error}")))?;
    }

    Ok(ExtractedRelease {
        binary_path,
        web_dist_dir: found_web.then_some(web_dist_dir),
    })
}

fn replace_release_files(
    exe_path: &Path,
    web_dist_dir: &Path,
    extracted: ExtractedRelease,
) -> Result<(), AdminError> {
    let web_backup = backup_path_for(web_dist_dir);
    let web_replaced = if let Some(new_web) = extracted.web_dist_dir {
        replace_dir(web_dist_dir, &web_backup, &new_web)?;
        true
    } else {
        false
    };

    let binary_backup = backup_path_for(exe_path);
    if binary_backup.exists() {
        fs::remove_file(&binary_backup).map_err(|error| {
            AdminError::internal(format!("Failed to remove old binary backup: {error}"))
        })?;
    }
    if let Err(error) = fs::rename(exe_path, &binary_backup) {
        if web_replaced {
            let _ = restore_dir(web_dist_dir, &web_backup);
        }
        return Err(AdminError::internal(format!(
            "Binary backup failed: {error}"
        )));
    }
    if let Err(error) = fs::rename(&extracted.binary_path, exe_path) {
        let _ = fs::rename(&binary_backup, exe_path);
        if web_replaced {
            let _ = restore_dir(web_dist_dir, &web_backup);
        }
        return Err(AdminError::internal(format!(
            "Binary replace failed: {error}"
        )));
    }
    Ok(())
}

fn replace_dir(current: &Path, backup: &Path, replacement: &Path) -> Result<(), AdminError> {
    if backup.exists() {
        fs::remove_dir_all(backup).map_err(|error| {
            AdminError::internal(format!("Failed to remove old web backup: {error}"))
        })?;
    }
    if current.exists() {
        fs::rename(current, backup).map_err(|error| {
            AdminError::internal(format!("Failed to backup web assets: {error}"))
        })?;
    }
    fs::rename(replacement, current)
        .map_err(|error| AdminError::internal(format!("Failed to replace web assets: {error}")))
}

fn restore_dir(current: &Path, backup: &Path) -> io::Result<()> {
    if current.exists() {
        fs::remove_dir_all(current)?;
    }
    if backup.exists() {
        fs::rename(backup, current)?;
    }
    Ok(())
}

fn backup_path_for(path: &Path) -> PathBuf {
    let mut backup = path.as_os_str().to_os_string();
    backup.push(".backup");
    PathBuf::from(backup)
}

fn web_dist_relative_path(path: &Path) -> Option<PathBuf> {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    for index in 0..components.len().saturating_sub(1) {
        if components[index] == "web"
            && components
                .get(index + 1)
                .is_some_and(|value| value == "dist")
        {
            return Some(components[index + 2..].iter().collect());
        }
        if components[index] == "dist" {
            return Some(components[index + 1..].iter().collect());
        }
    }
    None
}

fn unsafe_archive_path(path: &Path) -> bool {
    path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
}

fn validate_download_url(raw_url: &str) -> Result<(), AdminError> {
    let url = reqwest::Url::parse(raw_url)
        .map_err(|error| AdminError::bad_request(format!("Invalid download URL: {error}")))?;
    if url.scheme() == "http"
        && env_string("CPR_UPDATE_ALLOW_INSECURE_DOWNLOADS").as_deref() == Some("true")
    {
        return Ok(());
    }
    if url.scheme() != "https" {
        return Err(AdminError::bad_request(
            "Only HTTPS release downloads are allowed",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| AdminError::bad_request("Download URL is missing host"))?;
    let allowed = host == "github.com"
        || host.ends_with(".github.com")
        || host == "objects.githubusercontent.com"
        || host.ends_with(".objects.githubusercontent.com");
    if allowed {
        Ok(())
    } else {
        Err(AdminError::bad_request(format!(
            "Download host is not allowed: {host}"
        )))
    }
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

fn read_state(path: &Path) -> Result<SystemUpdateStatusData, AdminError> {
    if !path.exists() {
        return Ok(SystemUpdateStatusData::default());
    }
    let data = fs::read_to_string(path)
        .map_err(|error| AdminError::internal(format!("Failed to read update state: {error}")))?;
    serde_json::from_str(&data)
        .map_err(|error| AdminError::internal(format!("Invalid update state: {error}")))
}

fn write_state(path: &Path, state: &SystemUpdateStatusData) -> Result<(), AdminError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            AdminError::internal(format!("Failed to create update state directory: {error}"))
        })?;
    }
    let data = serde_json::to_string_pretty(state)
        .map_err(|error| AdminError::internal(format!("Failed to encode update state: {error}")))?;
    fs::write(path, data)
        .map_err(|error| AdminError::internal(format!("Failed to write update state: {error}")))
}

fn set_operation_running(
    path: &Path,
    operation_id: &str,
    kind: SystemOperationKind,
    version: Option<&str>,
) -> Result<(), AdminError> {
    let mut state = read_state(path)?;
    state.operation = SystemOperationState {
        operation_id: Some(operation_id.to_string()),
        kind: Some(kind),
        status: SystemOperationStatus::Running,
        target_version: version.map(ToString::to_string),
        message: Some("operation running".to_string()),
        error: None,
        started_at: Some(Utc::now().to_rfc3339()),
        finished_at: None,
    };
    write_state(path, &state)
}

fn finish_operation(
    path: &Path,
    operation_id: &str,
    kind: SystemOperationKind,
    version: Option<String>,
    error: Option<String>,
) {
    let mut state = match read_state(path) {
        Ok(state) => state,
        Err(error) => {
            eprintln!("failed to read update state: {error}");
            return;
        }
    };
    if state.operation.operation_id.as_deref() != Some(operation_id) {
        return;
    }

    if let Some(error) = error {
        state.operation.status = SystemOperationStatus::Failed;
        state.operation.message = Some("operation failed".to_string());
        state.operation.error = Some(error);
    } else {
        state.operation.status = SystemOperationStatus::Succeeded;
        state.operation.message = Some("operation succeeded".to_string());
        state.operation.error = None;
        if kind == SystemOperationKind::Update {
            state.previous_version = state.current_version.take();
            state.current_version = version.clone();
        }
        state.operation.target_version = version;
    }
    state.operation.finished_at = Some(Utc::now().to_rfc3339());

    if let Err(error) = write_state(path, &state) {
        eprintln!("failed to write update operation state: {error}");
    }
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

fn download_percent(downloaded: u64, total_size: u64) -> u64 {
    if total_size == 0 {
        return 0;
    }
    downloaded
        .saturating_mul(100)
        .saturating_div(total_size)
        .min(100)
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

fn normalize_version_tag(version: &str) -> String {
    version.trim().trim_start_matches('v').to_string()
}

fn operation_id(kind: &str) -> String {
    format!("sysop-{kind}-{}", Utc::now().timestamp_millis())
}

impl OperationLock {
    fn acquire(path: &Path) -> Result<Self, AdminError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| {
                AdminError::internal(format!("Failed to create update lock directory: {error}"))
            })?;
        }

        match Self::try_create(path) {
            Ok(()) => Ok(Self {
                path: path.to_path_buf(),
            }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if stale_lock(path)? {
                    fs::remove_file(path).map_err(|error| {
                        AdminError::internal(format!("Failed to remove stale update lock: {error}"))
                    })?;
                    Self::try_create(path).map_err(|error| {
                        AdminError::internal(format!("Failed to create update lock: {error}"))
                    })?;
                    return Ok(Self {
                        path: path.to_path_buf(),
                    });
                }
                Err(AdminError::conflict("System update already running"))
            }
            Err(error) => Err(AdminError::internal(format!(
                "Failed to create update lock: {error}"
            ))),
        }
    }

    fn try_create(path: &Path) -> io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        writeln!(
            file,
            "pid={}\ncreated_at={}",
            std::process::id(),
            Utc::now().to_rfc3339()
        )
    }
}

impl Drop for OperationLock {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path) {
            if error.kind() != io::ErrorKind::NotFound {
                eprintln!("failed to remove update lock: {error}");
            }
        }
    }
}

fn stale_lock(path: &Path) -> Result<bool, AdminError> {
    let metadata = fs::metadata(path)
        .map_err(|error| AdminError::internal(format!("Failed to read update lock: {error}")))?;
    let modified = metadata.modified().map_err(|error| {
        AdminError::internal(format!("Failed to read update lock timestamp: {error}"))
    })?;
    modified
        .elapsed()
        .map(|age| age > Duration::from_secs(30 * 60))
        .map_err(|error| {
            AdminError::internal(format!("Failed to calculate update lock age: {error}"))
        })
}
