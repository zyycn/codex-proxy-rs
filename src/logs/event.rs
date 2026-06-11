use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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
pub struct EventLog {
    pub id: String,
    pub kind: String,
    pub level: EventLevel,
    pub message: String,
    pub created_at: String,
}

impl EventLog {
    pub fn new(kind: impl Into<String>, level: EventLevel, message: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            kind: kind.into(),
            level,
            message: message.into(),
            created_at: Utc::now().to_rfc3339(),
        }
    }
}
