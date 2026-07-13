//! 会话亲和领域类型。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::upstream::openai::protocol::responses::PreviousResponseScope;

/// 单个账号返回的 `cyber_policy` 证据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CyberPolicyFailureSnapshot {
    pub account_id: String,
    pub event: String,
    pub message: String,
    pub upstream_code: Option<String>,
}

/// 会话级 `cyber_policy` 换号状态。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CyberPolicySessionState {
    pub failed_account_ids: Vec<String>,
    pub last_failure: Option<CyberPolicyFailureSnapshot>,
    #[serde(default)]
    pub revision: String,
}

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
    pub created_at: DateTime<Utc>,
}
