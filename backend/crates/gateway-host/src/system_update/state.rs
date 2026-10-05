//! 系统操作状态文件、跨进程锁与临时目录

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use gateway_admin::model::system::{
    SystemOperationKind, SystemOperationState, SystemOperationStatus, SystemUpdateStatus,
};
use serde::{Deserialize, Serialize};

use super::installation::ReleaseFiles;
use super::{OperationError, SystemUpdateConfig, conflict, internal};

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedState {
    previous_version: Option<String>,
    current_version: Option<String>,
    #[serde(default)]
    current_files: Option<ReleaseFiles>,
    #[serde(default)]
    previous_files: Option<ReleaseFiles>,
    #[serde(default)]
    operation: PersistedOperation,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedOperation {
    operation_id: Option<String>,
    kind: Option<PersistedKind>,
    #[serde(default)]
    status: PersistedStatus,
    target_version: Option<String>,
    message: Option<String>,
    error: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum PersistedKind {
    Update,
    Rollback,
    Restart,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum PersistedStatus {
    #[default]
    Idle,
    Running,
    Succeeded,
    Failed,
}

pub(crate) struct OperationFileLock {
    path: PathBuf,
}

impl OperationFileLock {
    pub(crate) fn acquire(path: &Path) -> Result<Self, OperationError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| {
                internal(format!("failed to create update lock directory: {error}"))
            })?;
        }
        match Self::try_create(path) {
            Ok(()) => Ok(Self { path: path.into() }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !stale_lock(path)? {
                    return Err(conflict("system update already running"));
                }
                fs::remove_file(path).map_err(|error| {
                    internal(format!("failed to remove stale update lock: {error}"))
                })?;
                Self::try_create(path).map_err(|error| {
                    if error.kind() == io::ErrorKind::AlreadyExists {
                        conflict("system update already running")
                    } else {
                        internal(format!("failed to create update lock: {error}"))
                    }
                })?;
                Ok(Self { path: path.into() })
            }
            Err(error) => Err(internal(format!("failed to create update lock: {error}"))),
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
        )?;
        file.sync_all()
    }
}

impl Drop for OperationFileLock {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.path.display(), error = %error, "清理系统更新锁失败");
        }
    }
}

pub(crate) struct UpdateTempDir {
    path: PathBuf,
}

impl UpdateTempDir {
    pub(crate) fn create(parent: &Path) -> Result<Self, OperationError> {
        for attempt in 0..100_u8 {
            let path = parent.join(format!(
                ".codex-proxy-rs-update-{}-{attempt}",
                Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(internal(format!(
                        "failed to create update temp directory: {error}"
                    )));
                }
            }
        }
        Err(internal("failed to create unique update temp directory"))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for UpdateTempDir {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.path.display(), error = %error, "清理系统更新临时目录失败");
        }
    }
}

/// 更新任务拥有持久化终态的责任，Future 在取消或 panic 时析构也必须收尾
pub(crate) struct UpdateOperation {
    path: PathBuf,
    operation_id: String,
    target: String,
    completed: bool,
    _file_lock: OperationFileLock,
}

impl UpdateOperation {
    pub(crate) fn start(
        path: &Path,
        operation_id: &str,
        target: &str,
        current_version: &str,
        file_lock: OperationFileLock,
    ) -> Result<Self, OperationError> {
        set_running(
            path,
            operation_id,
            SystemOperationKind::Update,
            Some(target),
            current_version,
        )?;
        Ok(Self {
            path: path.to_owned(),
            operation_id: operation_id.to_owned(),
            target: target.to_owned(),
            completed: false,
            _file_lock: file_lock,
        })
    }

    pub(crate) fn complete(
        &mut self,
        result: &Result<ReleaseFiles, OperationError>,
    ) -> Result<(), OperationError> {
        finish(
            &self.path,
            &self.operation_id,
            SystemOperationKind::Update,
            result.as_ref().ok().map(|_| self.target.clone()),
            result.as_ref().ok().cloned(),
            result.as_ref().err().map(ToString::to_string),
        )?;
        self.completed = true;
        Ok(())
    }
}

impl Drop for UpdateOperation {
    fn drop(&mut self) {
        if !self.completed
            && let Err(error) = finish(
                &self.path,
                &self.operation_id,
                SystemOperationKind::Update,
                None,
                None,
                Some("更新任务已中断，请重新发起更新".to_owned()),
            )
        {
            tracing::warn!(error = %error, "收敛中断的系统更新状态失败");
        }
    }
}

/// 只在拿到进程内锁后调用，旧版本断连遗留且已无执行者的 running 不能永久保留
pub(crate) fn recover_interrupted(path: &Path, lock_path: &Path) -> Result<(), OperationError> {
    let state = read_persisted(path)?;
    if !matches!(state.operation.status, PersistedStatus::Running) {
        return Ok(());
    }
    let _lock = match OperationFileLock::acquire(lock_path) {
        Ok(lock) => lock,
        Err(error) if error.kind() == super::SystemOperationErrorKind::Conflict => return Ok(()),
        Err(error) => return Err(error),
    };
    if let (Some(operation_id), Some(kind)) = (state.operation.operation_id, state.operation.kind) {
        finish(
            path,
            &operation_id,
            kind.into(),
            None,
            None,
            Some("更新任务已中断，请重新发起更新".to_owned()),
        )?;
    }
    Ok(())
}

pub(crate) fn operation_id(kind: &str) -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    format!(
        "sysop-{kind}-{}-{}",
        Utc::now().timestamp_millis(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

pub(crate) fn set_running(
    path: &Path,
    operation_id: &str,
    kind: SystemOperationKind,
    target_version: Option<&str>,
    current_version: &str,
) -> Result<(), OperationError> {
    let mut state = read_persisted(path)?;
    if state.current_version.is_none() {
        state.current_version = Some(current_version.to_owned());
    }
    state.operation = PersistedOperation {
        operation_id: Some(operation_id.to_owned()),
        kind: Some(kind.into()),
        status: PersistedStatus::Running,
        target_version: target_version.map(ToOwned::to_owned),
        message: Some("operation running".to_owned()),
        error: None,
        started_at: Some(Utc::now().to_rfc3339()),
        finished_at: None,
    };
    write_persisted(path, &state)
}

pub(crate) fn finish(
    path: &Path,
    operation_id: &str,
    kind: SystemOperationKind,
    version: Option<String>,
    files: Option<ReleaseFiles>,
    error: Option<String>,
) -> Result<(), OperationError> {
    let mut state = read_persisted(path)?;
    if state.operation.operation_id.as_deref() != Some(operation_id) {
        return Ok(());
    }
    if let Some(error) = error {
        state.operation.status = PersistedStatus::Failed;
        state.operation.message = Some(error.clone());
        state.operation.error = Some(error);
    } else {
        state.operation.status = PersistedStatus::Succeeded;
        state.operation.message = Some("operation succeeded".to_owned());
        state.operation.error = None;
        match kind {
            SystemOperationKind::Update => {
                state.previous_version = state.current_version.take();
                state.current_version = version.clone();
                state.previous_files = state.current_files.take();
                state.current_files = files;
            }
            SystemOperationKind::Rollback => {
                let current = state.current_version.take();
                state.current_version = state.previous_version.take();
                state.previous_version = current;
                std::mem::swap(&mut state.current_files, &mut state.previous_files);
            }
            SystemOperationKind::Restart => {}
        }
        state.operation.target_version = version;
    }
    state.operation.finished_at = Some(Utc::now().to_rfc3339());
    write_persisted(path, &state)
}

pub(crate) fn read_status(
    path: &Path,
    need_restart: bool,
) -> Result<SystemUpdateStatus, OperationError> {
    let state = read_persisted(path)?;
    let operation = state.operation;
    Ok(SystemUpdateStatus {
        need_restart,
        previous_version: state.previous_version,
        current_version: state.current_version,
        operation: SystemOperationState {
            operation_id: operation.operation_id,
            kind: operation.kind.map(Into::into),
            status: operation.status.into(),
            target_version: operation.target_version,
            message: operation.message,
            error: operation.error,
            started_at: parse_time(operation.started_at),
            finished_at: parse_time(operation.finished_at),
        },
    })
}

/// 调用方必须持有进程内锁及文件锁，避免将安装中间态当成手动部署
pub(crate) fn reconcile_installation(
    config: &SystemUpdateConfig,
    running: &ReleaseFiles,
) -> Result<SystemUpdateStatus, OperationError> {
    let path = &config.update_state_file;
    let mut state = read_persisted(path)?;
    let installed = ReleaseFiles::installed(config)?;
    let need_restart = &installed != running;
    if need_restart
        && (state.current_files.as_ref() != Some(&installed) || state.current_version.is_none())
    {
        return Err(conflict(
            "运行期间安装文件被外部修改，请核对部署并重启服务后重试",
        ));
    }
    let mut changed = false;
    if !need_restart
        && (state.current_files.as_ref() != Some(&installed)
            || state.current_version.as_deref() != Some(config.version.as_str()))
    {
        // 保留操作历史，但外部部署的文件不能继续使用旧操作推断版本
        state.current_version = Some(config.version.clone());
        state.current_files = Some(installed);
        changed = true;
    }
    if state.previous_version.is_some() || state.previous_files.is_some() {
        let backup_valid = state.previous_files.as_ref().is_some_and(|expected| {
            ReleaseFiles::backup(config)
                .as_ref()
                .is_ok_and(|actual| actual == expected)
        });
        if !backup_valid {
            // 无法证明旧备份完整时撤销回滚资格，不删除用户的备份文件
            state.previous_version = None;
            state.previous_files = None;
            changed = true;
        }
    }
    if changed {
        write_persisted(path, &state)?;
    }
    // 指纹证明磁盘仍是本进程安装的候选，回滚到旧版本也必须等待重启
    read_status(path, need_restart)
}

pub(crate) fn default_temp_dir(state_file: &Path) -> PathBuf {
    state_file
        .parent()
        .map(|parent| parent.join("update-tmp"))
        .unwrap_or_else(|| std::env::temp_dir().join("codex-proxy-rs-update"))
}

fn stale_lock(path: &Path) -> Result<bool, OperationError> {
    let modified = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| internal(format!("failed to read update lock timestamp: {error}")))?;
    modified
        .elapsed()
        .map(|age| age > Duration::from_secs(30 * 60))
        .map_err(|error| internal(format!("failed to calculate update lock age: {error}")))
}

fn read_persisted(path: &Path) -> Result<PersistedState, OperationError> {
    if !path.exists() {
        return Ok(PersistedState::default());
    }
    let data = fs::read_to_string(path)
        .map_err(|error| internal(format!("failed to read update state: {error}")))?;
    serde_json::from_str(&data).map_err(|error| internal(format!("invalid update state: {error}")))
}

fn write_persisted(path: &Path, state: &PersistedState) -> Result<(), OperationError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            internal(format!("failed to create update state directory: {error}"))
        })?;
    }
    let data = serde_json::to_vec_pretty(state)
        .map_err(|error| internal(format!("failed to encode update state: {error}")))?;
    let temporary = temporary_path(path);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&data)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| internal(format!("failed to write update state: {error}")))
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    PathBuf::from(temporary)
}

fn parse_time(value: Option<String>) -> Option<DateTime<Utc>> {
    value
        .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
        .map(|value| value.with_timezone(&Utc))
}

impl From<SystemOperationKind> for PersistedKind {
    fn from(value: SystemOperationKind) -> Self {
        match value {
            SystemOperationKind::Update => Self::Update,
            SystemOperationKind::Rollback => Self::Rollback,
            SystemOperationKind::Restart => Self::Restart,
        }
    }
}

impl From<PersistedKind> for SystemOperationKind {
    fn from(value: PersistedKind) -> Self {
        match value {
            PersistedKind::Update => Self::Update,
            PersistedKind::Rollback => Self::Rollback,
            PersistedKind::Restart => Self::Restart,
        }
    }
}

impl From<PersistedStatus> for SystemOperationStatus {
    fn from(value: PersistedStatus) -> Self {
        match value {
            PersistedStatus::Idle => Self::Idle,
            PersistedStatus::Running => Self::Running,
            PersistedStatus::Succeeded => Self::Succeeded,
            PersistedStatus::Failed => Self::Failed,
        }
    }
}
