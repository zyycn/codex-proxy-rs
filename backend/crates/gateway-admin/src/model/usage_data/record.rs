//! 既有请求明细的用量读写合同，不承载插件任务或来源状态

use super::UsageCount;
use serde::{Deserialize, Serialize};

/// Token 使用宿主口径：缓存包含在输入中，推理包含在输出中，未知值为 null
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageTokens {
    pub input_tokens: Option<UsageCount>,
    pub output_tokens: Option<UsageCount>,
    pub cached_input_tokens: Option<UsageCount>,
    pub cache_write_input_tokens: Option<UsageCount>,
    pub reasoning_output_tokens: Option<UsageCount>,
    pub image_input_tokens: Option<UsageCount>,
    pub image_output_tokens: Option<UsageCount>,
    pub total_tokens: Option<UsageCount>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageCostSource {
    ProviderReported,
    Calculated,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UsageCost {
    #[default]
    Unavailable,
    Known {
        source: UsageCostSource,
        amount: String,
        currency: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageOutcome {
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageRecordData {
    pub client_key_ref: String,
    pub provider: Option<String>,
    pub account_ref: Option<String>,
    pub requested_model: Option<String>,
    pub model: Option<String>,
    pub protocol: String,
    pub operation: String,
    pub endpoint: String,
    pub client_transport: String,
    pub service_tier: Option<String>,
    pub started_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub outcome: UsageOutcome,
    pub tokens: UsageTokens,
    pub cost: UsageCost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageRecord {
    pub id: String,
    pub data: UsageRecordData,
    pub billing_snapshot: Option<serde_json::Value>,
    pub provider_observation: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UsageRecordChange {
    Put {
        id: String,
        data: Box<UsageRecordData>,
    },
    /// tokens 替换全部 Token 字段；cost 的 unavailable 明确清空费用
    Update {
        id: String,
        tokens: Option<UsageTokens>,
        cost: Option<UsageCost>,
    },
    Delete {
        id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteUsageRecords {
    pub changes: Vec<UsageRecordChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageWriteResult {
    pub changed_ids: Vec<String>,
}
