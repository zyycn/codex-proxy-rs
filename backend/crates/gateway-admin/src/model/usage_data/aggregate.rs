//! 按归属时间和维度维护统计贡献，业务来源与调整历史由调用方保存

use super::{UsageDelta, UsageRecordFilter};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageMetric {
    RequestCount,
    SuccessCount,
    FailureCount,
    CancelledCount,
    IncompleteCount,
    CompletedRequestCount,
    InputTokens,
    OutputTokens,
    CachedInputTokens,
    CacheWriteInputTokens,
    ReasoningOutputTokens,
    ImageInputTokens,
    ImageOutputTokens,
    TotalTokens,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageDimensions {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub account_ref: Option<String>,
    pub client_key_ref: Option<String>,
    pub operation: Option<String>,
    pub transport: Option<String>,
}

/// 整个键决定覆盖目标；调用方可将来源标识编码进 key，重试相同键不会累计
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageAggregateKey {
    pub bucket_start_ms: i64,
    pub dimensions: UsageDimensions,
    pub key: String,
}

/// 值按 bucket_start_ms 归属，不自动摊分；负值用于相对原生统计的修正
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageAggregate {
    pub identity: UsageAggregateKey,
    pub counts: BTreeMap<UsageMetric, UsageDelta>,
    pub costs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UsageAggregateChange {
    Put { aggregate: UsageAggregate },
    Delete { identity: UsageAggregateKey },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteUsageAggregates {
    pub changes: Vec<UsageAggregateChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageAggregateWriteResult {
    pub changed: Vec<UsageAggregateKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListUsageAggregates {
    pub filter: UsageRecordFilter,
    pub cursor: Option<String>,
    pub limit: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageAggregatePage {
    pub items: Vec<UsageAggregate>,
    pub next_cursor: Option<String>,
}
