//! 系统版本、自更新与重启领域服务。

use std::{
    env, fmt, fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use chrono::Utc;
use serde::Serialize;
use tokio::sync::{Mutex, broadcast};

use super::{
    archive::{extract_release_archive, replace_release_files, rollback_release_update},
    download::{
        DownloadProgress, download_file, validate_download_url, validate_github_api_base,
        verify_checksum,
    },
    release::{
        GitHubRelease, ReleaseCache, UpdateInfoData, check_latest_release, fetch_latest_release,
        normalize_version_tag, select_release_archive, update_info_from_release,
    },
    state::{
        OperationLock, SystemOperationKind, SystemUpdateStatusData, finish_operation, operation_id,
        read_state, set_operation_running,
    },
    types::UpdateError,
};

pub(super) const APP_BINARY_NAME: &str = "codex-proxy-rs";
const DEFAULT_WEB_DIST_DIR: &str = "/app/web/dist";
const GITHUB_API_BASE: &str = "https://api.github.com/repos";
const MAX_DOWNLOAD_SIZE: u64 = 500 * 1024 * 1024;
const RESTART_DELAY_ENV: &str = "CPR_RESTART_DELAY_MS";
const REPLACEMENT_START_DELAY_MS: &str = "1200";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VersionData {
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateStartedData {
    operation_id: String,
    deployment_mode: String,
    message: String,
    need_restart: bool,
    target_version: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum UpdateLogLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SystemUpdateEvent {
    id: String,
    operation_id: Option<String>,
    level: UpdateLogLevel,
    step: Option<String>,
    message: String,
    #[serde(skip_serializing_if = "is_false")]
    terminal: bool,
    at: String,
}

impl SystemUpdateEvent {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.terminal
    }
}

#[derive(Debug, Clone)]
pub struct SystemUpdateConfig {
    pub version: String,
    pub git_sha: String,
    pub build_time: String,
    pub deployment_mode: String,
    pub build_type: String,
    pub update_channel: String,
    pub update_repository: Option<String>,
    pub github_api_base: String,
    pub executable_path: Option<PathBuf>,
    pub web_dist_dir: PathBuf,
    pub update_state_file: PathBuf,
    pub update_lock_file: PathBuf,
    pub update_temp_dir: PathBuf,
    pub self_restart_enabled: bool,
}

pub struct SystemUpdateService {
    config: SystemUpdateConfig,
    operation_lock: Mutex<()>,
    release_cache: ReleaseCache,
    events: UpdateEventSender,
}

impl SystemUpdateService {
    pub fn new(config: SystemUpdateConfig) -> Self {
        Self {
            config,
            operation_lock: Mutex::const_new(()),
            release_cache: ReleaseCache::default(),
            events: UpdateEventSender::default(),
        }
    }

    pub(crate) fn from_env() -> Self {
        Self::new(SystemUpdateConfig::from_env())
    }

    pub(crate) async fn version_data(&self) -> VersionData {
        let update_info = check_latest_release(&self.release_cache, &self.config, false).await;
        self.config.version_data(&update_info)
    }

    pub(crate) async fn update_detail(&self, refresh: bool) -> UpdateInfoData {
        check_latest_release(&self.release_cache, &self.config, refresh).await
    }

    pub(crate) fn subscribe_update_events(&self) -> broadcast::Receiver<SystemUpdateEvent> {
        self.events.subscribe()
    }

    pub(crate) async fn perform_update(
        &self,
        target_version: Option<String>,
    ) -> Result<UpdateStartedData, UpdateError> {
        let _guard = self
            .operation_lock
            .try_lock()
            .map_err(|_| UpdateError::conflict("System update already running"))?;
        let config = &self.config;
        if let Some(reason) = config.update_support_error() {
            self.events.emit_terminal(
                UpdateLogLevel::Error,
                None,
                Some("preflight"),
                reason.clone(),
            );
            return Err(UpdateError::conflict(reason));
        }
        let confirmed_target_version = confirmed_update_target(target_version)?;

        self.events.emit(
            UpdateLogLevel::Info,
            None,
            Some("release"),
            "正在获取最新 Release 信息",
        );
        let release = fetch_latest_release(
            &config.github_api_base,
            config.update_repository.as_deref().ok_or_else(|| {
                UpdateError::conflict("Update checks require CPR_UPDATE_REPOSITORY")
            })?,
        )
        .await
        .map_err(|error| {
            self.events
                .emit_terminal(UpdateLogLevel::Error, None, Some("release"), error.clone());
            UpdateError::bad_gateway(error)
        })?;
        let info = update_info_from_release(config, release.clone());
        if info.latest_version != confirmed_target_version {
            let message = format!(
                "远端最新版本已变更为 v{}，请重新检查并确认",
                info.latest_version
            );
            self.events.emit_terminal(
                UpdateLogLevel::Warning,
                None,
                Some("release"),
                message.clone(),
            );
            return Err(UpdateError::conflict(message));
        }
        if !info.has_update {
            self.events.emit_terminal(
                UpdateLogLevel::Warning,
                None,
                Some("release"),
                "当前版本已是最新",
            );
            return Err(UpdateError::conflict("Already up to date"));
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

        self.events.emit(
            UpdateLogLevel::Info,
            Some(&operation_id),
            Some("prepare"),
            format!("准备更新到 v{target_version}"),
        );
        let result = self
            .perform_release_update(&release, &target_version, &operation_id)
            .await;
        if let Err(error) = &result {
            self.events.emit_terminal(
                UpdateLogLevel::Error,
                Some(&operation_id),
                Some("failed"),
                error.to_string(),
            );
        } else {
            self.events.emit_terminal(
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

        Ok(UpdateStartedData {
            operation_id,
            deployment_mode: config.deployment_mode.clone(),
            message: "更新完成，请重启服务。".to_string(),
            need_restart: true,
            target_version,
        })
    }
}

fn confirmed_update_target(target_version: Option<String>) -> Result<String, UpdateError> {
    let Some(target_version) = target_version else {
        return Err(UpdateError::conflict("更新前需要确认目标版本"));
    };
    let target_version = normalize_version_tag(&target_version);
    if target_version.is_empty() {
        return Err(UpdateError::bad_request("目标版本不能为空"));
    }
    Ok(target_version)
}

impl SystemUpdateService {
    pub(crate) fn update_status(&self) -> Result<SystemUpdateStatusData, UpdateError> {
        read_state(&self.config.update_state_file)
    }

    pub(crate) async fn rollback(&self) -> Result<String, UpdateError> {
        let _guard = self
            .operation_lock
            .try_lock()
            .map_err(|_| UpdateError::conflict("系统操作正在执行中"))?;
        let config = &self.config;
        if let Some(reason) = config.update_support_error() {
            return Err(UpdateError::conflict(reason));
        }

        let operation_id = operation_id("rollback");
        let lock = OperationLock::acquire(&config.update_lock_file)?;
        set_operation_running(
            &config.update_state_file,
            &operation_id,
            SystemOperationKind::Rollback,
            None,
        )?;
        let result = rollback_release_update(config).await;
        finish_operation(
            &config.update_state_file,
            &operation_id,
            SystemOperationKind::Rollback,
            None,
            result.as_ref().err().map(ToString::to_string),
        );
        drop(lock);
        result?;

        Ok(operation_id)
    }

    pub(crate) fn restart_plan(&self) -> Result<RestartPlan, UpdateError> {
        if !self.config.self_restart_enabled {
            return Err(UpdateError::conflict(
                "自重启未启用，请设置 CPR_ENABLE_SELF_RESTART=true",
            ));
        }

        schedule_restart(&self.config)
    }
}

pub(crate) struct RestartPlan {
    pub(crate) message: &'static str,
    pub(crate) action: RestartAction,
}

pub(crate) enum RestartAction {
    Exec(PathBuf),
    Shutdown,
}

fn schedule_restart(config: &SystemUpdateConfig) -> Result<RestartPlan, UpdateError> {
    if config.deployment_mode == "docker" {
        return Ok(RestartPlan {
            message: "已安排进程内重启",
            action: RestartAction::Exec(config.executable_path()?),
        });
    }

    spawn_replacement_process(config)?;
    Ok(RestartPlan {
        message: "已安排自重启",
        action: RestartAction::Shutdown,
    })
}

fn spawn_replacement_process(config: &SystemUpdateConfig) -> Result<(), UpdateError> {
    let executable_path = config.executable_path()?;
    let mut command = Command::new(executable_path);
    command
        .args(env::args_os().skip(1))
        .env(RESTART_DELAY_ENV, REPLACEMENT_START_DELAY_MS);
    command.spawn().map(|_| ()).map_err(internal_error_with(
        "Failed to schedule replacement process",
    ))
}

impl SystemUpdateConfig {
    fn from_env() -> Self {
        let update_repository = env_string("CPR_UPDATE_REPOSITORY");
        let deployment_mode =
            env_string("CPR_DEPLOYMENT_MODE").unwrap_or_else(|| "source".to_string());
        let executable_path = env_string("CPR_UPDATE_EXE_PATH")
            .map(PathBuf::from)
            .or_else(|| {
                (deployment_mode == "docker")
                    .then(|| PathBuf::from("/app/bin").join(APP_BINARY_NAME))
            });
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
            deployment_mode,
            build_type: build_type(),
            update_channel: env_string("CPR_UPDATE_CHANNEL")
                .unwrap_or_else(|| "stable".to_string()),
            update_repository,
            github_api_base: env_string("CPR_GITHUB_API_BASE")
                .unwrap_or_else(|| GITHUB_API_BASE.to_string()),
            executable_path,
            web_dist_dir: env_string("CPR_WEB_DIST_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_WEB_DIST_DIR)),
            update_state_file: state_file,
            update_lock_file,
            update_temp_dir,
            self_restart_enabled: env_string("CPR_ENABLE_SELF_RESTART").as_deref() == Some("true"),
        }
    }

    fn version_data(&self, update_info: &UpdateInfoData) -> VersionData {
        VersionData {
            version: self.version.clone(),
            git_sha: self.git_sha.clone(),
            build_time: self.build_time.clone(),
            deployment_mode: self.deployment_mode.clone(),
            deployment_mode_label: deployment_mode_label(&self.deployment_mode).to_string(),
            update_channel: self.update_channel.clone(),
            latest_version: update_info.latest_version.clone(),
            has_update: update_info.has_update,
            update_cached: update_info.cached,
            update_warning: update_info.warning.clone(),
        }
    }

    pub(super) fn update_support_error(&self) -> Option<String> {
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

    pub(super) fn release_cache_key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.update_repository.as_deref().unwrap_or_default(),
            self.version,
            self.deployment_mode,
            self.build_type,
            self.update_channel,
        )
    }

    pub(super) fn executable_path(&self) -> Result<PathBuf, UpdateError> {
        if let Some(path) = &self.executable_path {
            return Ok(path.clone());
        }
        env::current_exe()
            .and_then(fs::canonicalize)
            .map_err(internal_error_with("Failed to resolve executable"))
    }
}

pub(super) fn internal_error(context: &str, error: impl fmt::Display) -> UpdateError {
    UpdateError::internal(format!("{context}: {error}"))
}

pub(super) fn internal_error_with<E: fmt::Display>(
    context: &'static str,
) -> impl FnOnce(E) -> UpdateError {
    move |error| internal_error(context, error)
}

pub(super) fn bad_request_with<E: fmt::Display>(
    context: &'static str,
) -> impl FnOnce(E) -> UpdateError {
    move |error| UpdateError::bad_request(format!("{context}: {error}"))
}

pub(super) fn bad_gateway_with<E: fmt::Display>(
    context: &'static str,
) -> impl FnOnce(E) -> UpdateError {
    move |error| UpdateError::bad_gateway(format!("{context}: {error}"))
}

impl SystemUpdateService {
    async fn perform_release_update(
        &self,
        release: &GitHubRelease,
        version: &str,
        operation_id: &str,
    ) -> Result<(), UpdateError> {
        let config = &self.config;
        self.events.emit(
            UpdateLogLevel::Info,
            Some(operation_id),
            Some("asset"),
            "正在选择匹配当前平台的更新包",
        );
        let archive = select_release_archive(release, version)?;
        self.events.emit(
            UpdateLogLevel::Info,
            Some(operation_id),
            Some("asset"),
            format!(
                "已选择更新包 {} ({})",
                archive.name,
                format_bytes(archive.size)
            ),
        );

        self.events.emit(
            UpdateLogLevel::Info,
            Some(operation_id),
            Some("verify"),
            "正在校验下载地址",
        );
        validate_download_url(&archive.browser_download_url, &config.github_api_base)?;
        if archive.size > MAX_DOWNLOAD_SIZE {
            return Err(UpdateError::bad_request("Release archive is too large"));
        }
        let checksum = release
            .assets
            .iter()
            .find(|asset| asset.name == "checksums.txt")
            .ok_or_else(|| UpdateError::bad_gateway("Release checksums.txt is required"))?;
        validate_download_url(&checksum.browser_download_url, &config.github_api_base)?;

        self.events.emit(
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

        self.events.emit(
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
                events: &self.events,
            }),
        )
        .await?;
        self.events.emit(
            UpdateLogLevel::Success,
            Some(operation_id),
            Some("download"),
            "更新包下载完成",
        );

        self.events.emit(
            UpdateLogLevel::Info,
            Some(operation_id),
            Some("checksum"),
            "正在校验 checksum",
        );
        verify_checksum(&archive_path, &archive.name, &checksum.browser_download_url).await?;
        self.events.emit(
            UpdateLogLevel::Success,
            Some(operation_id),
            Some("checksum"),
            "checksum 校验通过",
        );

        self.events.emit(
            UpdateLogLevel::Info,
            Some(operation_id),
            Some("extract"),
            "正在解压更新包",
        );
        let extracted = extract_release_archive(&archive_path, &temp_dir)?;
        self.events.emit(
            UpdateLogLevel::Success,
            Some(operation_id),
            Some("extract"),
            "更新包解压完成",
        );

        self.events.emit(
            UpdateLogLevel::Info,
            Some(operation_id),
            Some("replace"),
            "正在替换应用文件",
        );
        let exe_path = config.executable_path()?;
        replace_release_files(&exe_path, &config.web_dist_dir, extracted)?;
        self.events.emit(
            UpdateLogLevel::Success,
            Some(operation_id),
            Some("replace"),
            "应用文件替换完成",
        );
        let _ = fs::remove_dir_all(temp_dir);
        Ok(())
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

fn build_version() -> String {
    option_env!("CPR_VERSION")
        .unwrap_or(env!("CARGO_PKG_VERSION"))
        .to_string()
}

fn build_git_sha() -> String {
    option_env!("CPR_GIT_SHA").unwrap_or("unknown").to_string()
}

fn build_time() -> String {
    option_env!("CPR_BUILD_TIME")
        .unwrap_or("unknown")
        .to_string()
}

fn build_type() -> String {
    option_env!("CPR_BUILD_TYPE")
        .unwrap_or("source")
        .to_string()
}

pub(super) struct UpdateEventSender {
    sender: broadcast::Sender<SystemUpdateEvent>,
}

impl Default for UpdateEventSender {
    fn default() -> Self {
        let (sender, _receiver) = broadcast::channel(256);
        Self { sender }
    }
}

impl UpdateEventSender {
    fn subscribe(&self) -> broadcast::Receiver<SystemUpdateEvent> {
        self.sender.subscribe()
    }

    pub(super) fn emit(
        &self,
        level: UpdateLogLevel,
        operation_id: Option<&str>,
        step: Option<&str>,
        message: impl Into<String>,
    ) {
        self.emit_with_terminal(level, operation_id, step, message, false);
    }

    fn emit_terminal(
        &self,
        level: UpdateLogLevel,
        operation_id: Option<&str>,
        step: Option<&str>,
        message: impl Into<String>,
    ) {
        self.emit_with_terminal(level, operation_id, step, message, true);
    }

    fn emit_with_terminal(
        &self,
        level: UpdateLogLevel,
        operation_id: Option<&str>,
        step: Option<&str>,
        message: impl Into<String>,
        terminal: bool,
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
            terminal,
            at: now.to_rfc3339(),
        };
        let _ = self.sender.send(event);
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub(super) fn format_bytes(bytes: u64) -> String {
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

pub(super) fn deployment_mode_label(value: &str) -> &str {
    match value {
        "docker" => "Docker",
        "source" => "源码部署",
        "binary" => "二进制部署",
        _ => value,
    }
}

pub(super) fn build_type_label(value: &str) -> &str {
    match value {
        "release" => "正式构建",
        "source" => "源码构建",
        "dev" => "开发构建",
        _ => value,
    }
}
