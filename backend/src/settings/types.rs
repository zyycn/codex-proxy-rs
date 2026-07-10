use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 可热更新的运行时设置快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsSnapshot {
    pub model_aliases: BTreeMap<String, String>,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u32,
    pub max_concurrent_per_account: usize,
    pub request_interval_ms: u64,
    pub rotation_strategy: String,
}

impl Default for SettingsSnapshot {
    fn default() -> Self {
        Self {
            model_aliases: BTreeMap::new(),
            refresh_margin_seconds: 3600,
            refresh_concurrency: 2,
            max_concurrent_per_account: 3,
            request_interval_ms: 50,
            rotation_strategy: "smart".to_string(),
        }
    }
}

/// 管理端设置补丁。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub model_aliases: Option<BTreeMap<String, String>>,
    pub refresh_margin_seconds: Option<u64>,
    pub refresh_concurrency: Option<u32>,
    pub max_concurrent_per_account: Option<usize>,
    pub request_interval_ms: Option<u64>,
    pub rotation_strategy: Option<String>,
}

/// 管理员 API Key 状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementApiKeyStatus {
    pub exists: bool,
}

/// 设置字段校验错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SettingsValidationError {
    #[error("invalid setting `{field}`: {message}")]
    InvalidField { field: String, message: String },
}

impl SettingsValidationError {
    pub fn field(&self) -> &str {
        match self {
            Self::InvalidField { field, .. } => field,
        }
    }
}

/// 运行时设置服务错误。
#[derive(Debug, Error)]
pub enum SettingsError {
    #[error(transparent)]
    InvalidField(#[from] SettingsValidationError),
    #[error("runtime settings database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("runtime settings json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid stored setting `{field}`: {message}")]
    StoredField { field: String, message: String },
}
