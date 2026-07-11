//! 会话亲和领域类型。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::upstream::openai::protocol::responses::PreviousResponseScope;

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
    pub continuation_scope: PreviousResponseScope,
    pub replay: Option<ResponseReplaySnapshot>,
    pub created_at: DateTime<Utc>,
}

/// 截止某个 completed response 的完整、无凭据业务输入快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseReplaySnapshot {
    pub full_input: Vec<Value>,
}
