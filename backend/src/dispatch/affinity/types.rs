//! 会话亲和领域类型。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 会话亲和性条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAffinityEntry {
    pub account_id: String,
    pub conversation_id: String,
    pub turn_state: Option<String>,
    pub instructions_hash: Option<String>,
    pub input_tokens: Option<u64>,
    pub function_call_ids: Vec<String>,
    pub variant_hash: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// 从请求上下文派生的 conversation identity。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationIdentity {
    pub conversation_id: Option<String>,
    pub window_id: Option<String>,
}
