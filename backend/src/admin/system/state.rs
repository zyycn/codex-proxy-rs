//! 系统自更新状态与文件锁。

use std::{
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::admin::response::AdminError;

#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SystemUpdateStatusData {
    pub previous_version: Option<String>,
    pub current_version: Option<String>,
    #[serde(default)]
    pub operation: SystemOperationState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SystemOperationKind {
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
pub(crate) struct SystemOperationState {
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
pub(crate) struct OperationLock {
    path: PathBuf,
}

pub(crate) fn read_state(path: &Path) -> Result<SystemUpdateStatusData, AdminError> {
    if !path.exists() {
        return Ok(SystemUpdateStatusData::default());
    }
    let data =
        fs::read_to_string(path).map_err(internal_error_with("Failed to read update state"))?;
    serde_json::from_str(&data).map_err(internal_error_with("Invalid update state"))
}

fn write_state(path: &Path, state: &SystemUpdateStatusData) -> Result<(), AdminError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(internal_error_with(
            "Failed to create update state directory",
        ))?;
    }
    let data = serde_json::to_string_pretty(state)
        .map_err(internal_error_with("Failed to encode update state"))?;
    fs::write(path, data).map_err(internal_error_with("Failed to write update state"))
}

pub(crate) fn set_operation_running(
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

pub(crate) fn finish_operation(
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

pub(crate) fn operation_id(kind: &str) -> String {
    format!("sysop-{kind}-{}", Utc::now().timestamp_millis())
}

impl OperationLock {
    pub(crate) fn acquire(path: &Path) -> Result<Self, AdminError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(internal_error_with(
                "Failed to create update lock directory",
            ))?;
        }

        match Self::try_create(path) {
            Ok(()) => Ok(Self {
                path: path.to_path_buf(),
            }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if stale_lock(path)? {
                    fs::remove_file(path)
                        .map_err(internal_error_with("Failed to remove stale update lock"))?;
                    Self::try_create(path)
                        .map_err(internal_error_with("Failed to create update lock"))?;
                    return Ok(Self {
                        path: path.to_path_buf(),
                    });
                }
                Err(AdminError::conflict("System update already running"))
            }
            Err(error) => Err(internal_error("Failed to create update lock", error)),
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
    let metadata = fs::metadata(path).map_err(internal_error_with("Failed to read update lock"))?;
    let modified = metadata
        .modified()
        .map_err(internal_error_with("Failed to read update lock timestamp"))?;
    modified
        .elapsed()
        .map(|age| age > Duration::from_secs(30 * 60))
        .map_err(internal_error_with("Failed to calculate update lock age"))
}

fn internal_error(context: &str, error: impl std::fmt::Display) -> AdminError {
    AdminError::internal(format!("{context}: {error}"))
}

fn internal_error_with<E: std::fmt::Display>(
    context: &'static str,
) -> impl FnOnce(E) -> AdminError {
    move |error| internal_error(context, error)
}
