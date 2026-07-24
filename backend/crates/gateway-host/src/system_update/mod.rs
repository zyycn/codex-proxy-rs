//! 版本、安全自更新、回滚与进程重启的 Host-owned 实现。

mod archive;
mod download;
mod process;
mod release;
mod state;
mod swap;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use gateway_admin::model::system::{
    SystemOperationAccepted, SystemOperationKind, SystemUpdateDetail, SystemUpdateEvent,
    SystemUpdateEventLevel, SystemUpdateStatus, SystemVersion,
};
use gateway_admin::ports::system::{
    SystemOperationError, SystemOperationErrorKind, SystemOperations, SystemUpdateEventStream,
};
use gateway_core::engine::CancellationToken;
use serde::Deserialize;
use tokio::sync::{Mutex as AsyncMutex, broadcast};

use crate::config::ConfigError;

use self::archive::extract_release;
use self::download::{MAX_CHECKSUM_SIZE, MAX_DOWNLOAD_SIZE, download_file, verify_checksum};
use self::process::{environment_value, spawn_replacement};
use self::release::{
    ReleaseCache, confirmed_target, detail_from_release, fetch_latest, select_archive,
};
use self::state::{
    OperationFileLock, UpdateTempDir, finish, operation_id, read_status, set_running,
};
use self::swap::{replace_release_files, rollback_release};

const APP_BINARY_NAME: &str = "codex-proxy-rs";
const DEFAULT_WEB_DIST_DIR: &str = "/app/web/dist";
const DEFAULT_GITHUB_API_BASE: &str = "https://api.github.com/repos";
const DEFAULT_UPDATE_REPOSITORY: &str = "zyycn/codex-proxy-rs";

type OperationError = SystemOperationError;

/// 系统更新与重启配置；所有字段只由 Host 解释。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
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

impl Default for SystemUpdateConfig {
    fn default() -> Self {
        let deployment_mode =
            environment_value("CPR_DEPLOYMENT_MODE").unwrap_or_else(|| "source".to_owned());
        let executable_path = environment_value("CPR_UPDATE_EXE_PATH")
            .map(PathBuf::from)
            .or_else(|| {
                (deployment_mode == "docker")
                    .then(|| PathBuf::from("/app/bin").join(APP_BINARY_NAME))
            });
        let update_state_file = environment_value("CPR_UPDATE_STATE_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/app/.runtime/data/update-state.json"));
        let update_lock_file = environment_value("CPR_UPDATE_LOCK_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| update_state_file.with_extension("lock"));
        let update_temp_dir = environment_value("CPR_UPDATE_TEMP_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| state::default_temp_dir(&update_state_file));
        Self {
            version: option_env!("CPR_VERSION")
                .unwrap_or(env!("CARGO_PKG_VERSION"))
                .to_owned(),
            git_sha: option_env!("CPR_GIT_SHA").unwrap_or("unknown").to_owned(),
            build_time: option_env!("CPR_BUILD_TIME")
                .unwrap_or("unknown")
                .to_owned(),
            deployment_mode,
            build_type: option_env!("CPR_BUILD_TYPE").unwrap_or("source").to_owned(),
            update_channel: environment_value("CPR_UPDATE_CHANNEL")
                .unwrap_or_else(|| "stable".to_owned()),
            update_repository: Some(
                environment_value("CPR_UPDATE_REPOSITORY")
                    .unwrap_or_else(|| DEFAULT_UPDATE_REPOSITORY.to_owned()),
            ),
            github_api_base: environment_value("CPR_GITHUB_API_BASE")
                .unwrap_or_else(|| DEFAULT_GITHUB_API_BASE.to_owned()),
            executable_path,
            web_dist_dir: environment_value("CPR_WEB_DIST_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_WEB_DIST_DIR)),
            update_state_file,
            update_lock_file,
            update_temp_dir,
            self_restart_enabled: environment_value("CPR_ENABLE_SELF_RESTART").as_deref()
                == Some("true"),
        }
    }
}

impl SystemUpdateConfig {
    pub(crate) fn resolve_and_validate(&mut self, source_dir: &Path) -> Result<(), ConfigError> {
        for path in [
            self.executable_path.as_mut(),
            Some(&mut self.web_dist_dir),
            Some(&mut self.update_state_file),
            Some(&mut self.update_lock_file),
            Some(&mut self.update_temp_dir),
        ]
        .into_iter()
        .flatten()
        {
            if path.is_relative() {
                *path = source_dir.join(&*path);
            }
        }
        if self.version.trim().is_empty() {
            return Err(ConfigError::InvalidField("host.system_update.version"));
        }
        if !matches!(
            self.deployment_mode.as_str(),
            "source" | "binary" | "docker"
        ) {
            return Err(ConfigError::InvalidField(
                "host.system_update.deployment_mode",
            ));
        }
        if !matches!(self.update_channel.as_str(), "stable" | "preview") {
            return Err(ConfigError::InvalidField(
                "host.system_update.update_channel",
            ));
        }
        Ok(())
    }

    fn update_support_error(&self) -> Option<String> {
        if self.build_type != "release" {
            return Some("一键更新需要正式构建包".to_owned());
        }
        let Some(repository) = self.update_repository.as_deref() else {
            return Some("检查更新需要配置 CPR_UPDATE_REPOSITORY".to_owned());
        };
        if let Err(error) = release::validate_repository(repository) {
            return Some(error.to_string());
        }
        if let Err(error) = release::validate_api_base(&self.github_api_base) {
            return Some(error);
        }
        None
    }

    fn release_cache_key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.update_repository.as_deref().unwrap_or_default(),
            self.github_api_base,
            self.version,
            self.deployment_mode,
            self.build_type,
            self.update_channel,
        )
    }

    pub(crate) fn executable_path(&self) -> Result<PathBuf, OperationError> {
        if let Some(path) = &self.executable_path {
            return Ok(path.clone());
        }
        env::current_exe()
            .and_then(fs::canonicalize)
            .map_err(|error| internal(format!("failed to resolve executable: {error}")))
    }
}

/// gateway-admin 消费的真实进程/文件系统操作实现。
pub struct ProcessSystemOperations {
    cancellation: CancellationToken,
    events: UpdateEvents,
    config: SystemUpdateConfig,
    operation_lock: AsyncMutex<()>,
    release_cache: ReleaseCache,
}

impl ProcessSystemOperations {
    #[must_use]
    pub fn new(cancellation: CancellationToken, config: SystemUpdateConfig) -> Self {
        Self {
            cancellation,
            events: UpdateEvents::default(),
            config,
            operation_lock: AsyncMutex::const_new(()),
            release_cache: ReleaseCache::default(),
        }
    }

    async fn perform_update_inner(
        &self,
        target_version: Option<String>,
    ) -> Result<SystemOperationAccepted, OperationError> {
        let _operation = self
            .operation_lock
            .try_lock()
            .map_err(|_| conflict("system operation is already running"))?;
        if let Some(reason) = self.config.update_support_error() {
            self.events
                .error_terminal(None, Some("preflight"), reason.clone());
            return Err(conflict(reason));
        }
        let target = confirmed_target(target_version)?;
        let repository = self
            .config
            .update_repository
            .as_deref()
            .ok_or_else(|| conflict("update repository is not configured"))?;
        self.events
            .info(None, Some("release"), "fetching latest release");
        let release = fetch_latest(&self.config.github_api_base, repository).await?;
        let detail = detail_from_release(&self.config, &release);
        if detail.latest_version != target {
            self.events.warning_terminal(
                None,
                Some("release"),
                "remote latest version changed; confirm again",
            );
            return Err(conflict("remote latest version changed"));
        }
        if !detail.has_update {
            self.events
                .warning_terminal(None, Some("release"), "already up to date");
            return Err(conflict("already up to date"));
        }

        let operation_id = operation_id("update");
        let file_lock = OperationFileLock::acquire(&self.config.update_lock_file)?;
        set_running(
            &self.config.update_state_file,
            &operation_id,
            SystemOperationKind::Update,
            Some(&target),
            &self.config.version,
        )?;
        let result = self.install_release(&release, &target, &operation_id).await;
        match &result {
            Ok(()) => self.events.success_terminal(
                Some(&operation_id),
                Some("done"),
                "release files replaced",
            ),
            Err(error) => {
                self.events
                    .error_terminal(Some(&operation_id), Some("failed"), error.to_string())
            }
        }
        finish(
            &self.config.update_state_file,
            &operation_id,
            SystemOperationKind::Update,
            result.as_ref().ok().map(|()| target.clone()),
            result.as_ref().err().map(ToString::to_string),
        );
        drop(file_lock);
        result?;
        Ok(SystemOperationAccepted::Update {
            operation_id,
            deployment_mode: self.config.deployment_mode.clone(),
            message: "更新完成，请重启服务。".to_owned(),
            need_restart: true,
            target_version: target,
        })
    }

    async fn install_release(
        &self,
        release: &release::GitHubRelease,
        version: &str,
        operation_id: &str,
    ) -> Result<(), OperationError> {
        let archive = select_archive(release, version)?;
        if archive.size == 0 || archive.size > MAX_DOWNLOAD_SIZE {
            return Err(invalid("release archive size is invalid"));
        }
        let checksum = release
            .assets
            .iter()
            .find(|asset| asset.name == "checksums.txt")
            .ok_or_else(|| upstream("release checksums.txt is required"))?;
        if checksum.size == 0 || checksum.size > MAX_CHECKSUM_SIZE {
            return Err(invalid("release checksum size is invalid"));
        }
        fs::create_dir_all(&self.config.update_temp_dir)
            .map_err(|error| internal(format!("failed to prepare update temp dir: {error}")))?;
        let temp_root = fs::canonicalize(&self.config.update_temp_dir)
            .map_err(|error| internal(format!("failed to resolve update temp dir: {error}")))?;
        let temp = UpdateTempDir::create(&temp_root)?;
        let archive_path = temp.path().join(&archive.name);
        download_file(
            &archive.browser_download_url,
            &archive_path,
            archive.size,
            &self.config.github_api_base,
            operation_id,
            &self.events,
        )
        .await?;
        verify_checksum(
            &archive_path,
            &archive.name,
            &checksum.browser_download_url,
            checksum.size,
            &self.config.github_api_base,
        )
        .await?;
        let extracted = extract_release(&archive_path, temp.path())?;
        replace_release_files(
            &self.config.executable_path()?,
            &self.config.web_dist_dir,
            extracted,
        )
    }
}

#[async_trait]
impl SystemOperations for ProcessSystemOperations {
    async fn version(&self) -> Result<SystemVersion, OperationError> {
        let detail = self.update_detail(false).await?;
        Ok(SystemVersion {
            version: self.config.version.clone(),
            git_sha: self.config.git_sha.clone(),
            build_time: self.config.build_time.clone(),
            deployment_mode: self.config.deployment_mode.clone(),
            update_channel: self.config.update_channel.clone(),
            latest_version: detail.latest_version,
            has_update: detail.has_update,
            update_cached: detail.cached,
            update_warning: detail.warning,
        })
    }

    async fn update_detail(&self, refresh: bool) -> Result<SystemUpdateDetail, OperationError> {
        match self.release_cache.detail(&self.config, refresh).await {
            Ok(detail) => Ok(detail),
            Err(error) => Ok(base_update_detail(
                &self.config,
                self.config.update_support_error(),
                Some(error.to_string()),
            )),
        }
    }

    fn update_events(&self) -> SystemUpdateEventStream {
        let receiver = self.events.subscribe();
        Box::pin(futures::stream::unfold(
            (receiver, false),
            |(mut receiver, close)| async move {
                if close {
                    return None;
                }
                loop {
                    match receiver.recv().await {
                        Ok((event, terminal)) => return Some((event, (receiver, terminal))),
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
            },
        ))
    }

    async fn perform_update(
        &self,
        target_version: Option<String>,
    ) -> Result<SystemOperationAccepted, OperationError> {
        self.perform_update_inner(target_version).await
    }

    async fn update_status(&self) -> Result<SystemUpdateStatus, OperationError> {
        read_status(&self.config.update_state_file)
    }

    async fn rollback(&self) -> Result<SystemOperationAccepted, OperationError> {
        let _operation = self
            .operation_lock
            .try_lock()
            .map_err(|_| conflict("system operation is already running"))?;
        if let Some(reason) = self.config.update_support_error() {
            return Err(conflict(reason));
        }
        let operation_id = operation_id("rollback");
        let file_lock = OperationFileLock::acquire(&self.config.update_lock_file)?;
        set_running(
            &self.config.update_state_file,
            &operation_id,
            SystemOperationKind::Rollback,
            None,
            &self.config.version,
        )?;
        let result = rollback_release(&self.config);
        match &result {
            Ok(()) => self.events.success_terminal(
                Some(&operation_id),
                Some("done"),
                "previous release restored",
            ),
            Err(error) => {
                self.events
                    .error_terminal(Some(&operation_id), Some("failed"), error.to_string())
            }
        }
        finish(
            &self.config.update_state_file,
            &operation_id,
            SystemOperationKind::Rollback,
            None,
            result.as_ref().err().map(ToString::to_string),
        );
        drop(file_lock);
        result?;
        Ok(SystemOperationAccepted::Rollback {
            operation_id,
            message: "回滚完成，请重启服务。".to_owned(),
            need_restart: true,
        })
    }

    async fn restart(&self) -> Result<SystemOperationAccepted, OperationError> {
        if !self.config.self_restart_enabled {
            return Err(conflict("self restart is disabled"));
        }
        let message = if self.config.deployment_mode == "docker" {
            "已安排进程内重启"
        } else {
            spawn_replacement(&self.config)?;
            "已安排自重启"
        };
        let operation_id = operation_id("restart");
        let cancellation = self.cancellation.clone();
        drop(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            cancellation.cancel();
        }));
        Ok(SystemOperationAccepted::Restart {
            operation_id,
            message: message.to_owned(),
        })
    }
}

fn base_update_detail(
    config: &SystemUpdateConfig,
    unsupported_reason: Option<String>,
    warning: Option<String>,
) -> SystemUpdateDetail {
    SystemUpdateDetail {
        current_version: config.version.clone(),
        latest_version: config.version.clone(),
        has_update: false,
        deployment_mode: config.deployment_mode.clone(),
        build_type: config.build_type.clone(),
        release_url: None,
        notes: None,
        cached: false,
        update_supported: unsupported_reason.is_none() && warning.is_none(),
        unsupported_reason,
        warning,
    }
}

pub(crate) struct UpdateEvents {
    sender: broadcast::Sender<(SystemUpdateEvent, bool)>,
    sequence: AtomicU64,
}

impl Default for UpdateEvents {
    fn default() -> Self {
        let (sender, _) = broadcast::channel(256);
        Self {
            sender,
            sequence: AtomicU64::new(0),
        }
    }
}

impl UpdateEvents {
    fn subscribe(&self) -> broadcast::Receiver<(SystemUpdateEvent, bool)> {
        self.sender.subscribe()
    }

    fn info(&self, operation_id: Option<&str>, step: Option<&str>, message: impl Into<String>) {
        self.emit(
            SystemUpdateEventLevel::Info,
            operation_id,
            step,
            message,
            None,
            false,
        );
    }

    fn warning_terminal(
        &self,
        operation_id: Option<&str>,
        step: Option<&str>,
        message: impl Into<String>,
    ) {
        self.emit(
            SystemUpdateEventLevel::Warning,
            operation_id,
            step,
            message,
            None,
            true,
        );
    }

    fn success_terminal(
        &self,
        operation_id: Option<&str>,
        step: Option<&str>,
        message: impl Into<String>,
    ) {
        self.emit(
            SystemUpdateEventLevel::Success,
            operation_id,
            step,
            message,
            Some(100),
            true,
        );
    }

    fn error_terminal(
        &self,
        operation_id: Option<&str>,
        step: Option<&str>,
        message: impl Into<String>,
    ) {
        self.emit(
            SystemUpdateEventLevel::Error,
            operation_id,
            step,
            message,
            None,
            true,
        );
    }

    pub(crate) fn emit_progress(&self, operation_id: &str, message: &str, progress: u8) {
        self.emit(
            SystemUpdateEventLevel::Info,
            Some(operation_id),
            Some("download"),
            message,
            Some(progress),
            false,
        );
    }

    fn emit(
        &self,
        level: SystemUpdateEventLevel,
        operation_id: Option<&str>,
        step: Option<&str>,
        message: impl Into<String>,
        progress_percent: Option<u8>,
        terminal: bool,
    ) {
        let now = Utc::now();
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let event = SystemUpdateEvent {
            id: format!(
                "update-log-{}-{sequence}",
                now.timestamp_nanos_opt()
                    .unwrap_or_else(|| now.timestamp_millis())
            ),
            operation_id: operation_id.map(str::to_owned),
            level,
            step: step.map(str::to_owned),
            message: message.into(),
            terminal,
            progress_percent,
            occurred_at: now,
        };
        let _ = self.sender.send((event, terminal));
    }
}

fn invalid(message: impl Into<String>) -> OperationError {
    SystemOperationError::new(SystemOperationErrorKind::Invalid, message)
}

fn conflict(message: impl Into<String>) -> OperationError {
    SystemOperationError::new(SystemOperationErrorKind::Conflict, message)
}

fn upstream(message: impl Into<String>) -> OperationError {
    SystemOperationError::new(SystemOperationErrorKind::Upstream, message)
}

fn internal(message: impl Into<String>) -> OperationError {
    SystemOperationError::new(SystemOperationErrorKind::Internal, message)
}
