//! 历史用量的窄事实、分组统计与调用内分批读取合同

use super::{UsageCount, UsageMetric, UsageRecordData, UsageRecordFilter};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const USAGE_READ_MAX_BATCH_ROWS: u16 = 1024;
pub const USAGE_READ_MAX_BATCH_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageReadFilter {
    pub records: UsageRecordFilter,
    pub routing_group_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanUsageRecords {
    pub filter: UsageReadFilter,
    pub batch_size: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageGrouping {
    Model,
    Key,
    KeyModel,
    RoutingGroup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryUsageStatistics {
    pub filter: UsageReadFilter,
    pub group_by: UsageGrouping,
    pub batch_size: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UsageReadQuery {
    Records(ScanUsageRecords),
    Statistics(QueryUsageStatistics),
}

impl UsageReadQuery {
    pub fn filter(&self) -> &UsageReadFilter {
        match self {
            Self::Records(query) => &query.filter,
            Self::Statistics(query) => &query.filter,
        }
    }

    pub fn batch_size(&self) -> u16 {
        match self {
            Self::Records(query) => query.batch_size,
            Self::Statistics(query) => query.batch_size,
        }
        .unwrap_or(USAGE_READ_MAX_BATCH_ROWS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRecordFact {
    pub id: String,
    pub data: UsageRecordData,
    pub user_agent: Option<String>,
    pub reasoning_effort: Option<String>,
    pub subagent_kind: Option<String>,
    pub request_kind: Option<String>,
    pub routing_scope: Option<String>,
    pub routing_group_refs: Vec<String>,
    pub recovered: bool,
    /// 由宿主现行规则派生，原始审计中的恢复请求仍可读取
    pub request_counted: bool,
    /// 包含完整交付、预热排除及恢复关联规则
    pub usage_counted: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageStatisticsGroup {
    pub client_key_ref: Option<String>,
    pub model: Option<String>,
    pub routing_group_ref: Option<String>,
    pub unassigned_contribution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageStatisticsCost {
    pub currency: String,
    pub amount: String,
    pub native_amount: Option<String>,
    /// 外部修正允许负值，原生费用来源覆盖单独返回
    pub contribution_amount: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageStatisticsCostCoverage {
    pub provider_reported: UsageCount,
    pub calculated: UsageCount,
    pub unavailable: UsageCount,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageStatisticsMetrics {
    /// 缺失的 Token 指标表示未知，计数指标始终返回
    pub values: BTreeMap<UsageMetric, UsageCount>,
    pub native_request_count: UsageCount,
    pub contribution_count: UsageCount,
    pub native_token_samples: BTreeMap<UsageMetric, UsageCount>,
    /// 数量表示提供该指标的贡献记录数，不能用作请求覆盖率的分母
    pub contribution_token_samples: BTreeMap<UsageMetric, UsageCount>,
    pub cost_coverage: UsageStatisticsCostCoverage,
    pub costs: Vec<UsageStatisticsCost>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageStatisticsRow {
    /// null 表示全局总计，分组重叠不会重复计入该行
    pub group: Option<UsageStatisticsGroup>,
    pub metrics: UsageStatisticsMetrics,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum UsageReadItem {
    Record(Box<UsageRecordFact>),
    Statistics(Box<UsageStatisticsRow>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageReadAnalytics {
    pub coverage_start_ms: Option<i64>,
    pub coverage_end_ms: Option<i64>,
    pub lag_seconds: u64,
    pub state: String,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageReadObservation {
    pub generated_at_ms: i64,
    pub analytics: Option<UsageReadAnalytics>,
    pub total_groups: Option<UsageCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageReadBatch {
    pub observation: UsageReadObservation,
    pub items: Vec<UsageReadItem>,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageReadOpened {
    pub reader: Option<String>,
    pub batch: UsageReadBatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageReadHandle {
    pub reader: String,
}
