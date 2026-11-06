//! 系统更新、回滚与重启的受理、互斥和文件交换生命周期

use std::{fs, path::Path, sync::Arc, time::Duration};

use async_trait::async_trait;
use gateway_admin::model::system::{
    SystemOperationAccepted, SystemOperationKind, SystemUpdateChannel as UpdateChannel,
    SystemUpdateDetail, SystemUpdateStatus, SystemVersion,
};
use gateway_admin::ports::system::{
    SystemOperationErrorKind, SystemOperations, SystemRestartPreflight, SystemUpdateCandidate,
    SystemUpdateEventStream, SystemUpdatePreflight,
};
use gateway_core::lifecycle::CancellationToken;
use serde::Deserialize;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, broadcast};

use super::archive::extract_release;
use super::download::{
    MAX_CHECKSUM_SIZE, MAX_DOWNLOAD_SIZE, download_file, format_bytes, verify_checksum,
};
use super::events::UpdateEvents;
use super::installation::{OFFICIAL_PLUGIN_MANIFEST, ReleaseFiles};
use super::process::spawn_replacement;
use super::release::{
    GitHubRelease, ReleaseCache, base_update_detail, confirmed_target, detail_from_release,
    fetch_latest, select_archive, update_policy, validate_update_target, version_channel,
};
use super::state::{
    OperationFileLock, UpdateOperation, UpdateTempDir, finish, operation_id, read_status,
    reconcile_installation, recover_interrupted, set_running,
};
use super::swap::{replace_release_files, rollback_official_plugins_dir, rollback_release};
use super::{OperationError, SystemUpdateConfig, conflict, internal, invalid, upstream};

const MAX_OFFICIAL_PLUGIN_MANIFEST_SIZE: u64 = 256 * 1024;

/// gateway-admin 消费的真实进程/文件系统操作实现
#[derive(Clone)]
pub struct ProcessSystemOperations {
    cancellation: CancellationToken,
    events: Arc<UpdateEvents>,
    config: Arc<SystemUpdateConfig>,
    operation_lock: Arc<AsyncMutex<Option<RestartGuard>>>,
    release_cache: Arc<ReleaseCache>,
    running_files: Arc<Result<ReleaseFiles, OperationError>>,
    diagnostics: super::diagnostics::Diagnostics,
}

struct RestartGuard {
    // 字段按声明顺序释放，先移除更新锁，再通知替换进程开始启动
    _file_lock: OperationFileLock,
    _replacement_input: Option<std::process::ChildStdin>,
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
            operation_lock: Arc::new(AsyncMutex::new(None)),
            release_cache: Arc::new(ReleaseCache::default()),
            running_files,
            diagnostics: super::diagnostics::Diagnostics::default(),
        }
    }

    /// Store 就绪后绑定受控诊断端口，不改变 Host 的初始化顺序
    pub fn set_diagnostics(
        &self,
        diagnostics: Arc<dyn gateway_core::diagnostics::OperationalDiagnostics>,
    ) {
        self.diagnostics.bind(diagnostics);
    }

    fn lock_operation(&self) -> Result<OwnedMutexGuard<Option<RestartGuard>>, OperationError> {
        let operation = Arc::clone(&self.operation_lock)
            .try_lock_owned()
            .map_err(|_| conflict("已有系统操作正在执行"))?;
        if operation.is_some() {
            return Err(conflict("服务正在重启"));
        }
        if self.cancellation.is_cancelled() {
            return Err(conflict("服务正在关闭"));
        }
        Ok(operation)
    }

    fn start_update(
        &self,
        target_version: Option<String>,
        channel: Option<UpdateChannel>,
        preflight: Arc<dyn SystemUpdatePreflight>,
    ) -> Result<SystemOperationAccepted, OperationError> {
        let operation_lock = self.lock_operation()?;
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
                .error_terminal(None, Some("preflight"), error.message().to_owned());
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
            self.diagnostics.clone(),
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
            let completion = operation.complete(&result);
            if let Err(error) = &result {
                service
                    .diagnostics
                    .record("install", Some(&operation_id), error)
                    .await;
            }
            if let Err(error) = completion {
                service
                    .diagnostics
                    .record("persist_update_status", Some(&operation_id), &error)
                    .await;
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
                    error.message().to_owned(),
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
            .ok_or_else(|| conflict("未配置更新仓库 CPR_UPDATE_REPOSITORY"))?;
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
        release: &GitHubRelease,
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
                format_bytes(archive.size)
            ),
        );
        self.events
            .info(Some(operation_id), Some("verify"), "正在校验更新资源");
        if archive.size == 0 || archive.size > MAX_DOWNLOAD_SIZE {
            return Err(invalid("更新包大小无效或超出限制"));
        }
        let checksum = release
            .assets
            .iter()
            .find(|asset| asset.name == "checksums.txt")
            .ok_or_else(|| upstream("发行版本缺少 checksums.txt 校验文件，无法安全更新"))?;
        if checksum.size == 0 || checksum.size > MAX_CHECKSUM_SIZE {
            return Err(invalid("校验文件大小无效或超出限制"));
        }
        self.events
            .info(Some(operation_id), Some("prepare"), "正在创建临时更新目录");
        fs::create_dir_all(&self.config.update_temp_dir)
            .map_err(|error| internal("无法创建更新临时目录").with_source(error))?;
        let temp_root = fs::canonicalize(&self.config.update_temp_dir)
            .map_err(|error| internal("无法确定更新临时目录位置").with_source(error))?;
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
        let mut applied = AppliedReleaseGuard::new(&self.config, &self.diagnostics);
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
        // 重启就绪探测依赖本接口，不能等待可能跨多页的发行列表
        let channel = self.resolve_channel(None)?;
        let cached = self
            .release_cache
            .cached_detail(&self.config, channel)
            .await;
        let (latest_version, has_update, update_cached, update_warning) = match cached {
            Some(detail) => (
                detail.latest_version,
                detail.has_update,
                detail.cached,
                detail.warning,
            ),
            None => (self.config.version.clone(), false, false, None),
        };
        Ok(SystemVersion {
            version: self.config.version.clone(),
            git_sha: self.config.git_sha.clone(),
            build_time: self.config.build_time.clone(),
            deployment_mode: self.config.deployment_mode.clone(),
            update_channel: version_channel(&self.config.version)
                .map(|channel| channel.as_str().to_owned())
                .unwrap_or_else(|| "unknown".to_owned()),
            latest_version,
            has_update,
            update_cached,
            update_warning,
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
            Err(error) => {
                self.diagnostics.record("check_release", None, &error).await;
                Ok(base_update_detail(
                    &self.config,
                    channel,
                    self.config.update_support_error(),
                    Some(error.message().to_owned()),
                ))
            }
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
        if let Ok(_operation) = self.lock_operation() {
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
        let _operation = self.lock_operation()?;
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
                let mut applied = AppliedReleaseGuard::new(&self.config, &self.diagnostics);
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
            result
                .as_ref()
                .err()
                .map(|error| error.message().to_owned()),
        )?;
        match &result {
            Ok(()) => self.events.success_terminal(
                Some(&operation_id),
                Some("done"),
                "上一版本已恢复，请重启服务",
            ),
            Err(error) => self.events.error_terminal(
                Some(&operation_id),
                Some("failed"),
                error.message().to_owned(),
            ),
        }
        drop(file_lock);
        result?;
        Ok(SystemOperationAccepted::Rollback {
            operation_id,
            message: "回滚完成，请重启服务".to_owned(),
            need_restart: true,
        })
    }

    async fn restart_candidate(&self) -> Result<Option<SystemUpdateCandidate>, OperationError> {
        let _operation = self.lock_operation()?;
        self.installed_restart_candidate()
    }

    async fn restart(
        &self,
        preflight: Arc<dyn SystemRestartPreflight>,
    ) -> Result<SystemOperationAccepted, OperationError> {
        // 与 update/rollback 互斥：更新替换文件期间触发自重启会让新进程
        // 载入半成品产物
        let mut operation = self.lock_operation()?;
        if !self.config.self_restart_enabled {
            return Err(conflict("当前部署未启用在线重启"));
        }
        let file_lock = OperationFileLock::acquire(&self.config.update_lock_file)?;
        let candidate = self.installed_restart_candidate()?;
        let files = candidate
            .as_ref()
            .map(|_| ReleaseFiles::installed(&self.config))
            .transpose()?;
        preflight.prepare(candidate).await?;
        if self.cancellation.is_cancelled() {
            return Err(conflict("服务正在关闭"));
        }
        if let Some(files) = files
            && files != ReleaseFiles::installed(&self.config)?
        {
            return Err(conflict("重启检查期间安装文件已变化，请重新确认"));
        }
        let (message, replacement_input) = if self.config.deployment_mode == "docker" {
            ("已安排进程内重启", None)
        } else {
            ("已安排自重启", spawn_replacement(&self.config)?)
        };
        let operation_id = operation_id("restart");
        // 受理后直到旧服务释放，保留重启状态和跨进程锁；延迟关闭只用于送达响应
        *operation = Some(RestartGuard {
            _file_lock: file_lock,
            _replacement_input: replacement_input,
        });
        let cancellation = self.cancellation.clone();
        drop(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            // 留出受理响应返回时间，再走统一关闭流程
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
        .map_err(|error| invalid("无法访问官方插件发行清单").with_source(error))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(invalid("官方插件发行清单必须是普通文件"));
    }
    if metadata.len() == 0 || metadata.len() > MAX_OFFICIAL_PLUGIN_MANIFEST_SIZE {
        return Err(invalid("官方插件发行清单大小无效或超出限制"));
    }
    fs::read(path)
        .map(Arc::<[u8]>::from)
        .map_err(|error| invalid("无法读取官方插件发行清单").with_source(error))
}

fn restore_applied_release(config: &SystemUpdateConfig, cause: OperationError) -> OperationError {
    match rollback_release(config) {
        Ok(()) => cause,
        Err(error) => internal("更新校验失败，且无法恢复原文件")
            .with_source(gateway_core::error::ErrorSource::new(cause).with_cleanup(error)),
    }
}

struct AppliedReleaseGuard<'a> {
    config: &'a SystemUpdateConfig,
    diagnostics: &'a super::diagnostics::Diagnostics,
    armed: bool,
}

impl<'a> AppliedReleaseGuard<'a> {
    const fn new(
        config: &'a SystemUpdateConfig,
        diagnostics: &'a super::diagnostics::Diagnostics,
    ) -> Self {
        Self {
            config,
            diagnostics,
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
            self.diagnostics
                .record_on_drop("restore_cancelled_installation", None, error);
        }
    }
}
