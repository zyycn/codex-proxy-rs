//! 系统操作状态文件、跨进程锁与临时目录。

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use gateway_admin::model::system::{
    SystemOperationKind, SystemOperationState, SystemOperationStatus, SystemUpdateStatus,
};
use serde::{Deserialize, Serialize};

use super::{OperationError, conflict, internal};

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedState {
    previous_version: Option<String>,
    current_version: Option<String>,
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

pub(crate) fn operation_id(kind: &str) -> String {
    format!("sysop-{kind}-{}", Utc::now().timestamp_millis())
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
    error: Option<String>,
) {
    let mut state = match read_persisted(path) {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(error = %error, "读取系统更新状态失败");
            return;
        }
    };
    if state.operation.operation_id.as_deref() != Some(operation_id) {
        return;
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
            }
            SystemOperationKind::Rollback => {
                let current = state.current_version.take();
                state.current_version = state.previous_version.take();
                state.previous_version = current;
            }
            SystemOperationKind::Restart => {}
        }
        state.operation.target_version = version;
    }
    state.operation.finished_at = Some(Utc::now().to_rfc3339());
    if let Err(error) = write_persisted(path, &state) {
        tracing::warn!(error = %error, "写入系统更新状态失败");
    }
}

pub(crate) fn read_status(path: &Path) -> Result<SystemUpdateStatus, OperationError> {
    let state = read_persisted(path)?;
    let operation = state.operation;
    Ok(SystemUpdateStatus {
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
