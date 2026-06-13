use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EventLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl EventLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventLog {
    pub id: String,
    pub request_id: Option<String>,
    pub kind: String,
    pub level: EventLevel,
    pub account_id: Option<String>,
    pub route: Option<String>,
    pub model: Option<String>,
    pub status_code: Option<i64>,
    pub latency_ms: Option<i64>,
    pub message: String,
    pub metadata: Value,
    pub created_at: String,
}

impl EventLog {
    pub fn new(kind: impl Into<String>, level: EventLevel, message: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            request_id: None,
            kind: kind.into(),
            level,
            account_id: None,
            route: None,
            model: None,
            status_code: None,
            latency_ms: None,
            message: message.into(),
            metadata: json!({}),
            created_at: Utc::now().to_rfc3339(),
        }
    }
}
