//! 版本、安全自更新、回滚与进程重启的 Host-owned 实现

mod archive;
mod download;
mod installation;
mod process;
mod release;
mod state;
mod swap;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use gateway_admin::model::system::{
    SystemOperationAccepted, SystemOperationKind, SystemUpdateChannel as UpdateChannel,
    SystemUpdateDetail, SystemUpdateEvent, SystemUpdateEventLevel, SystemUpdatePolicy,
    SystemUpdateStatus, SystemVersion,
};
use gateway_admin::ports::system::{
    SystemOperationError, SystemOperationErrorKind, SystemOperations, SystemRestartPreflight,
    SystemUpdateCandidate, SystemUpdateEventStream, SystemUpdatePreflight,
};
use gateway_core::lifecycle::CancellationToken;
use serde::Deserialize;
use tokio::sync::{Mutex as AsyncMutex, broadcast};

use crate::config::ConfigError;

use self::archive::extract_release;
use self::download::{MAX_CHECKSUM_SIZE, MAX_DOWNLOAD_SIZE, download_file, verify_checksum};
use self::installation::ReleaseFiles;
use self::process::{environment_value, spawn_replacement};
use self::release::{
    ReleaseCache, confirmed_target, detail_from_release, fetch_latest, select_archive,
    validate_update_target, version_channel,
};
use self::state::{
    OperationFileLock, UpdateOperation, UpdateTempDir, finish, operation_id, read_status,
    reconcile_installation, recover_interrupted, set_running,
};
use self::swap::{replace_release_files, rollback_official_plugins_dir, rollback_release};

pub use self::release::validate_download_url;

const APP_BINARY_NAME: &str = "codex-proxy-rs";
const DEFAULT_GITHUB_API_BASE: &str = "https://api.github.com/repos";
const DEFAULT_UPDATE_REPOSITORY: &str = "zyycn/codex-proxy-rs";
const OFFICIAL_PLUGIN_MANIFEST: &str = "plugin-release-manifest.json";
const MAX_OFFICIAL_PLUGIN_MANIFEST_SIZE: u64 = 256 * 1024;

type OperationError = SystemOperationError;

/// 系统更新与重启配置；所有字段只由 Host 解释
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SystemUpdateConfig {
    pub version: String,
    pub git_sha: String,
    pub build_time: String,
    pub deployment_mode: String,
    pub build_type: String,
    pub update_repository: Option<String>,
    pub github_api_base: String,
    pub executable_path: Option<PathBuf>,
    /// 未显式指定时，由组合根传入 API 实际使用的静态资源目录
    pub web_dist_dir: Option<PathBuf>,
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
            .unwrap_or_else(|| PathBuf::from("update-state.json"));
        let update_lock_file = environment_value("CPR_UPDATE_LOCK_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| update_state_file.with_file_name("update.lock"));
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
            update_repository: Some(
                environment_value("CPR_UPDATE_REPOSITORY")
                    .unwrap_or_else(|| DEFAULT_UPDATE_REPOSITORY.to_owned()),
            ),
            github_api_base: environment_value("CPR_GITHUB_API_BASE")
                .unwrap_or_else(|| DEFAULT_GITHUB_API_BASE.to_owned()),
            executable_path,
            web_dist_dir: None,
            update_state_file,
            update_lock_file,
            update_temp_dir,
            self_restart_enabled: environment_value("CPR_ENABLE_SELF_RESTART").as_deref()
                == Some("true"),
        }
    }
}

impl SystemUpdateConfig {
    pub(crate) fn resolve_and_validate(
        &mut self,
        source_dir: &Path,
        runtime_data_dir: &Path,
        asset_directory: &Path,
    ) -> Result<(), ConfigError> {
        let web_dist_dir = self
            .web_dist_dir
            .get_or_insert_with(|| asset_directory.to_path_buf());
        if web_dist_dir.as_os_str().is_empty() {
            return Err(ConfigError::InvalidField("host.system_update.web_dist_dir"));
        }
        for path in [self.executable_path.as_mut(), self.web_dist_dir.as_mut()]
            .into_iter()
            .flatten()
        {
            if path.is_relative() {
                *path = source_dir.join(&*path);
            }
        }
        for path in [
            &mut self.update_state_file,
            &mut self.update_lock_file,
            &mut self.update_temp_dir,
        ] {
            if path.is_relative() {
                *path = runtime_data_dir.join(&*path);
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
        Ok(())
    }

    fn update_support_error(&self) -> Option<String> {
        if !matches!(self.build_type.as_str(), "release" | "experimental") {
            return Some("在线更新需要官方发布构建".to_owned());
        }
        let Some(channel) = version_channel(&self.version) else {
            return Some("当前版本不符合发行命名规范，无法确定更新通道".to_owned());
        };
        if self.build_type == "experimental" && channel != UpdateChannel::Experimental {
            return Some("实验构建必须使用 exp.N 版本，无法在线更新".to_owned());
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
        if let Err(error) = self.web_dist_dir() {
            return Some(error.to_string());
        }
        None
    }

    fn release_cache_key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.update_repository.as_deref().unwrap_or_default(),
            self.github_api_base,
            self.version,
            self.deployment_mode,
            self.build_type,
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

    pub(crate) fn official_plugins_dir(&self) -> Result<PathBuf, OperationError> {
        let executable = self.executable_path()?;
        let parent = executable
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| internal("failed to resolve official plugin release directory"))?;
        Ok(parent.join("plugins").join("official"))
    }

    pub(crate) fn web_dist_dir(&self) -> Result<&Path, OperationError> {
        self.web_dist_dir
            .as_deref()
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| conflict("web assets directory is not configured"))
    }
}

/// gateway-admin 消费的真实进程/文件系统操作实现
#[derive(Clone)]
pub struct ProcessSystemOperations {
    cancellation: CancellationToken,
    events: Arc<UpdateEvents>,
    config: Arc<SystemUpdateConfig>,
    operation_lock: Arc<AsyncMutex<()>>,
    release_cache: Arc<ReleaseCache>,
    running_files: Arc<Result<ReleaseFiles, OperationError>>,
}

impl ProcessSystemOperations {
    #[must_use]
    pub fn new(cancellation: CancellationToken, mut config: SystemUpdateConfig) -> Self {
        // 组合根在接收请求前固定安装路径和发行文件；旧程序重命名后，
        // current_exe 可能指向备份，后续更新、回滚和重启不能再反查运行文件
        let running_files = Arc::new(config.executable_path().and_then(|executable| {
            config.executable_path = Some(executable);
            ReleaseFiles::installed(&config)
        }));
        Self {
            cancellation,
            events: Arc::new(UpdateEvents::default()),
            config: Arc::new(config),
            operation_lock: Arc::new(AsyncMutex::new(())),
            release_cache: Arc::new(ReleaseCache::default()),
            running_files,
        }
    }

    fn start_update(
        &self,
        target_version: Option<String>,
        channel: Option<UpdateChannel>,
        preflight: Arc<dyn SystemUpdatePreflight>,
    ) -> Result<SystemOperationAccepted, OperationError> {
        let operation_lock = Arc::clone(&self.operation_lock)
            .try_lock_owned()
            .map_err(|_| conflict("system operation is already running"))?;
        if self.cancellation.is_cancelled() {
            return Err(conflict("服务正在关闭"));
        }
        if let Some(reason) = self.config.update_support_error() {
            self.events
                .error_terminal(None, Some("preflight"), reason.clone());
            return Err(conflict(reason));
        }
        let target = confirmed_target(target_version)?;
        let file_lock = OperationFileLock::acquire(&self.config.update_lock_file)?;
        let selected = self.resolve_channel(channel)?;
        if let Err(error) = validate_update_target(&self.config.version, &target, selected) {
            self.events
                .error_terminal(None, Some("preflight"), error.to_string());
            return Err(error);
        }
        let status = self.reconcile_installation()?;
        if status.need_restart {
            return Err(conflict("更新已完成，请先重启服务"));
        }
        let operation_id = operation_id("update");
        let mut operation = UpdateOperation::start(
            &self.config.update_state_file,
            &operation_id,
            &target,
            &self.config.version,
            file_lock,
        )?;
        let accepted = SystemOperationAccepted::Update {
            operation_id: operation_id.clone(),
            deployment_mode: self.config.deployment_mode.clone(),
            message: "更新已开始".to_owned(),
            target_version: target.clone(),
        };
        let service = self.clone();
        // 任务持有互斥锁和落盘守卫，HTTP 断开不会取消更新；Host 关闭仍会收敛终态
        drop(tokio::spawn(async move {
            let _operation_lock = operation_lock;
            let result = tokio::select! {
                biased;
                () = service.cancellation.cancelled() => Err(conflict("服务关闭，更新已中断")),
                result = service.perform_update_inner(&target, selected, &operation_id, preflight.as_ref()) => result,
            };
            if let Err(error) = operation.complete(&result) {
                tracing::warn!(error = %error, "系统更新终态落盘失败");
                return;
            }
            match result {
                Ok(_) => service.events.success_terminal(
                    Some(&operation_id),
                    Some("done"),
                    "更新文件已替换，等待服务重启生效",
                ),
                Err(error) => service.events.error_terminal(
                    Some(&operation_id),
                    Some("failed"),
                    error.to_string(),
                ),
            }
        }));
        Ok(accepted)
    }

    async fn perform_update_inner(
        &self,
        target: &str,
        channel: UpdateChannel,
        operation_id: &str,
        preflight: &dyn SystemUpdatePreflight,
    ) -> Result<ReleaseFiles, OperationError> {
        let repository = self
            .config
            .update_repository
            .as_deref()
            .ok_or_else(|| conflict("update repository is not configured"))?;
        self.events.info(
            Some(operation_id),
            Some("release"),
            "正在获取最新 Release 信息",
        );
        let release = fetch_latest(
            &self.config.github_api_base,
            repository,
            &self.config.version,
            channel,
        )
        .await?
        .ok_or_else(|| conflict("当前发行通道没有可用更新"))?;
        let detail = detail_from_release(&self.config, &release, channel);
        if detail.latest_version != target {
            return Err(conflict("远端最新版本已变化，请重新确认"));
        }
        if !detail.has_update {
            return Err(conflict("当前没有可用更新"));
        }
        self.events.info(
            Some(operation_id),
            Some("prepare"),
            format!("准备更新到 v{target}"),
        );
        self.install_release(&release, target, operation_id, preflight)
            .await
    }

    async fn install_release(
        &self,
        release: &release::GitHubRelease,
        version: &str,
        operation_id: &str,
        preflight: &dyn SystemUpdatePreflight,
    ) -> Result<ReleaseFiles, OperationError> {
        self.events.info(
            Some(operation_id),
            Some("asset"),
            "正在选择匹配当前平台的更新包",
        );
        let archive = select_archive(release, version)?;
        self.events.info(
            Some(operation_id),
            Some("asset"),
            format!(
                "已选择更新包 {} ({})",
                archive.name,
                download::format_bytes(archive.size)
            ),
        );
        self.events
            .info(Some(operation_id), Some("verify"), "正在校验更新资源");
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
        self.events
            .info(Some(operation_id), Some("prepare"), "正在创建临时更新目录");
        fs::create_dir_all(&self.config.update_temp_dir)
            .map_err(|error| internal(format!("failed to prepare update temp dir: {error}")))?;
        let temp_root = fs::canonicalize(&self.config.update_temp_dir)
            .map_err(|error| internal(format!("failed to resolve update temp dir: {error}")))?;
        let temp = UpdateTempDir::create(&temp_root)?;
        let archive_path = temp.path().join(&archive.name);
        self.events
            .info(Some(operation_id), Some("download"), "开始下载更新包");
        download_file(
            &archive.browser_download_url,
            &archive_path,
            archive.size,
            &self.config.github_api_base,
            operation_id,
            &self.events,
        )
        .await?;
        self.events
            .success(Some(operation_id), Some("download"), "更新包下载完成");
        self.events
            .info(Some(operation_id), Some("checksum"), "正在校验 checksum");
        verify_checksum(
            &archive_path,
            &archive.name,
            &checksum.browser_download_url,
            checksum.size,
            &self.config.github_api_base,
        )
        .await?;
        self.events
            .success(Some(operation_id), Some("checksum"), "checksum 校验通过");
        self.events
            .info(Some(operation_id), Some("extract"), "正在解压更新包");
        let extracted = extract_release(&archive_path, temp.path())?;
        self.events
            .success(Some(operation_id), Some("extract"), "更新包解压完成");
        let release_manifest = read_release_manifest(&extracted.official_plugins_dir)?;
        let candidate = SystemUpdateCandidate {
            target_version: version.to_owned(),
            release_manifest: release_manifest.clone(),
        };
        self.events.info(
            Some(operation_id),
            Some("preflight"),
            "正在校验目标发行信息",
        );
        let plugin_revision = preflight.validate(candidate).await?;
        preflight.confirm_revision(plugin_revision).await?;
        self.events.success(
            Some(operation_id),
            Some("preflight"),
            "目标发行信息校验通过，插件兼容性将在重启前检查",
        );
        self.events
            .info(Some(operation_id), Some("replace"), "正在替换应用文件");
        replace_release_files(
            &self.config.executable_path()?,
            self.config.web_dist_dir()?,
            extracted,
        )?;
        let mut applied = AppliedReleaseGuard::new(&self.config);
        let applied_manifest = read_release_manifest(&self.config.official_plugins_dir()?)?;
        if applied_manifest.as_ref() != release_manifest.as_ref() {
            return Err(applied.rollback(conflict("已应用的更新候选与预检候选不一致")));
        }
        if let Err(error) = preflight.confirm_revision(plugin_revision).await {
            return Err(applied.rollback(error));
        }
        let files = ReleaseFiles::installed(&self.config)?;
        applied.commit();
        self.events
            .success(Some(operation_id), Some("replace"), "应用文件替换完成");
        Ok(files)
    }

    fn resolve_channel(
        &self,
        requested: Option<UpdateChannel>,
    ) -> Result<UpdateChannel, OperationError> {
        let channel = requested.unwrap_or_else(|| {
            version_channel(&self.config.version).unwrap_or(UpdateChannel::Stable)
        });
        if !update_policy(&self.config, channel)
            .available_channels
            .contains(&channel)
        {
            return Err(conflict("当前发行版本不支持此更新通道"));
        }
        Ok(channel)
    }

    fn installed_restart_candidate(&self) -> Result<Option<SystemUpdateCandidate>, OperationError> {
        if self.config.build_type == "source" {
            return Ok(None);
        }
        let release_manifest = read_release_manifest(&self.config.official_plugins_dir()?)?;
        #[derive(Deserialize)]
        struct ReleaseIdentity {
            gateway_version: String,
        }
        let identity: ReleaseIdentity = serde_json::from_slice(&release_manifest)
            .map_err(|_| invalid("已安装发行清单缺少目标版本"))?;
        Ok(Some(SystemUpdateCandidate {
            target_version: identity.gateway_version,
            release_manifest,
        }))
    }

    fn reconcile_installation(&self) -> Result<SystemUpdateStatus, OperationError> {
        let running_files = self.running_files.as_ref().as_ref().map_err(Clone::clone)?;
        reconcile_installation(&self.config, running_files)
    }
}

#[async_trait]
impl SystemOperations for ProcessSystemOperations {
    async fn version(&self) -> Result<SystemVersion, OperationError> {
        let detail = self.update_detail(false, None).await?;
        Ok(SystemVersion {
            version: self.config.version.clone(),
            git_sha: self.config.git_sha.clone(),
            build_time: self.config.build_time.clone(),
            deployment_mode: self.config.deployment_mode.clone(),
            update_channel: version_channel(&self.config.version)
                .map(|channel| channel.as_str().to_owned())
                .unwrap_or_else(|| "unknown".to_owned()),
            latest_version: detail.latest_version,
            has_update: detail.has_update,
            update_cached: detail.cached,
            update_warning: detail.warning,
        })
    }

    async fn update_detail(
        &self,
        refresh: bool,
        channel: Option<UpdateChannel>,
    ) -> Result<SystemUpdateDetail, OperationError> {
        let channel = self.resolve_channel(channel)?;
        match self
            .release_cache
            .detail(&self.config, refresh, channel)
            .await
        {
            Ok(detail) => Ok(detail),
            Err(error) => Ok(base_update_detail(
                &self.config,
                channel,
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
        channel: Option<UpdateChannel>,
        preflight: Arc<dyn SystemUpdatePreflight>,
    ) -> Result<SystemOperationAccepted, OperationError> {
        self.start_update(target_version, channel, preflight)
    }

    async fn update_status(&self) -> Result<SystemUpdateStatus, OperationError> {
        if let Ok(_operation) = self.operation_lock.try_lock() {
            recover_interrupted(
                &self.config.update_state_file,
                &self.config.update_lock_file,
            )?;
            if self.config.update_support_error().is_none() {
                match OperationFileLock::acquire(&self.config.update_lock_file) {
                    Ok(_lock) => return self.reconcile_installation(),
                    Err(error) if error.kind() == SystemOperationErrorKind::Conflict => {}
                    Err(error) => return Err(error),
                }
            }
        }
        read_status(&self.config.update_state_file, false)
    }

    async fn rollback(
        &self,
        preflight: Arc<dyn SystemUpdatePreflight>,
    ) -> Result<SystemOperationAccepted, OperationError> {
        let _operation = self
            .operation_lock
            .try_lock()
            .map_err(|_| conflict("system operation is already running"))?;
        if let Some(reason) = self.config.update_support_error() {
            return Err(conflict(reason));
        }
        let operation_id = operation_id("rollback");
        let file_lock = OperationFileLock::acquire(&self.config.update_lock_file)?;
        let target_version = self
            .reconcile_installation()?
            .previous_version
            .ok_or_else(|| conflict("没有可用于回滚的上一版本"))?;
        let release_manifest =
            read_release_manifest(&rollback_official_plugins_dir(&self.config)?)?;
        let plugin_revision = preflight
            .validate_rollback(SystemUpdateCandidate {
                target_version,
                release_manifest: Arc::clone(&release_manifest),
            })
            .await?;
        preflight.confirm_revision(plugin_revision).await?;
        set_running(
            &self.config.update_state_file,
            &operation_id,
            SystemOperationKind::Rollback,
            None,
            &self.config.version,
        )?;
        let result = match rollback_release(&self.config) {
            Ok(()) => {
                let mut applied = AppliedReleaseGuard::new(&self.config);
                let result = match read_release_manifest(&self.config.official_plugins_dir()?) {
                    Ok(manifest) if manifest.as_ref() == release_manifest.as_ref() => {
                        preflight.confirm_revision(plugin_revision).await
                    }
                    Ok(_) => Err(conflict("已应用的回滚候选与预检候选不一致")),
                    Err(error) => Err(error),
                };
                match result {
                    Ok(()) => {
                        applied.commit();
                        Ok(())
                    }
                    Err(error) => Err(applied.rollback(error)),
                }
            }
            Err(error) => Err(error),
        };
        finish(
            &self.config.update_state_file,
            &operation_id,
            SystemOperationKind::Rollback,
            None,
            None,
            result.as_ref().err().map(ToString::to_string),
        )?;
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
        drop(file_lock);
        result?;
        Ok(SystemOperationAccepted::Rollback {
            operation_id,
            message: "回滚完成，请重启服务。".to_owned(),
            need_restart: true,
        })
    }

    async fn restart_candidate(&self) -> Result<Option<SystemUpdateCandidate>, OperationError> {
        let _operation = self
            .operation_lock
            .try_lock()
            .map_err(|_| conflict("system operation is already running"))?;
        self.installed_restart_candidate()
    }

    async fn restart(
        &self,
        preflight: Arc<dyn SystemRestartPreflight>,
    ) -> Result<SystemOperationAccepted, OperationError> {
        // 与 update/rollback 互斥：更新替换文件期间触发自重启会让新进程
        // 载入半成品产物
        let _operation = self
            .operation_lock
            .try_lock()
            .map_err(|_| conflict("system operation is already running"))?;
        if !self.config.self_restart_enabled {
            return Err(conflict("self restart is disabled"));
        }
        let _file_lock = OperationFileLock::acquire(&self.config.update_lock_file)?;
        let candidate = self.installed_restart_candidate()?;
        let files = candidate
            .as_ref()
            .map(|_| ReleaseFiles::installed(&self.config))
            .transpose()?;
        preflight.prepare(candidate).await?;
        if let Some(files) = files
            && files != ReleaseFiles::installed(&self.config)?
        {
            return Err(conflict("重启检查期间安装文件已变化，请重新确认"));
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

fn read_release_manifest(directory: &Path) -> Result<Arc<[u8]>, OperationError> {
    let path = directory.join(OFFICIAL_PLUGIN_MANIFEST);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| invalid(format!("official plugin manifest is unavailable: {error}")))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(invalid("official plugin manifest is not a regular file"));
    }
    if metadata.len() == 0 || metadata.len() > MAX_OFFICIAL_PLUGIN_MANIFEST_SIZE {
        return Err(invalid("official plugin manifest size is invalid"));
    }
    fs::read(path)
        .map(Arc::<[u8]>::from)
        .map_err(|error| invalid(format!("official plugin manifest cannot be read: {error}")))
}

fn restore_applied_release(config: &SystemUpdateConfig, cause: OperationError) -> OperationError {
    match rollback_release(config) {
        Ok(()) => cause,
        Err(error) => internal(format!(
            "release verification failed and restoring the previous files failed: {error}"
        )),
    }
}

struct AppliedReleaseGuard<'a> {
    config: &'a SystemUpdateConfig,
    armed: bool,
}

impl<'a> AppliedReleaseGuard<'a> {
    const fn new(config: &'a SystemUpdateConfig) -> Self {
        Self {
            config,
            armed: true,
        }
    }

    const fn commit(&mut self) {
        self.armed = false;
    }

    fn rollback(mut self, cause: OperationError) -> OperationError {
        self.armed = false;
        restore_applied_release(self.config, cause)
    }
}

impl Drop for AppliedReleaseGuard<'_> {
    fn drop(&mut self) {
        if self.armed
            && let Err(error) = rollback_release(self.config)
        {
            tracing::error!(error = %error, "系统发行物临界区取消后的恢复失败");
        }
    }
}

fn update_policy(config: &SystemUpdateConfig, channel: UpdateChannel) -> SystemUpdatePolicy {
    use UpdateChannel::{Alpha, Beta, Experimental, Rc, Stable};
    SystemUpdatePolicy {
        channel,
        available_channels: if version_channel(&config.version) == Some(Experimental) {
            vec![Experimental]
        } else {
            vec![Stable, Rc, Beta, Alpha]
        },
    }
}

fn base_update_detail(
    config: &SystemUpdateConfig,
    channel: UpdateChannel,
    unsupported_reason: Option<String>,
    warning: Option<String>,
) -> SystemUpdateDetail {
    SystemUpdateDetail {
        policy: update_policy(config, channel),
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

    fn success(&self, operation_id: Option<&str>, step: Option<&str>, message: impl Into<String>) {
        self.emit(
            SystemUpdateEventLevel::Success,
            operation_id,
            step,
            message,
            None,
            false,
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
