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

/// 单轮 completed response 的有界、无凭据重放增量。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseReplaySnapshot {
    /// 上一轮由本代理记录的 completed response。
    pub parent_response_id: Option<String>,
    /// 本轮客户端新增的输入，已剥离账号绑定字段。
    pub turn_input: Vec<Value>,
    /// 本轮上游输出，已剥离账号绑定字段和上游对象 ID。
    pub turn_output: Vec<Value>,
    /// 从会话起点到本节点的增量节点数。
    pub depth: u16,
    /// 从会话起点到本节点的 JSON 编码累计字节数。
    pub total_bytes: u64,
}
