//! 统计投影的新鲜度、覆盖范围与预计算费用合同

use chrono::{DateTime, Utc};

use super::observability::{DecimalAmount, RequestMetricPoint, UsageOverview};

/// 数据可见性状态，不以缺失投影冒充零统计
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AnalyticsState {
    Ready,
    #[default]
    Building,
    Lagging,
    Stale,
}

impl AnalyticsState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Building => "building",
            Self::Lagging => "lagging",
            Self::Stale => "stale",
        }
    }
}

/// 与统计结果在同一读取快照内取得的覆盖和版本事实
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnalyticsMetadata {
    pub coverage_start: Option<DateTime<Utc>>,
    pub coverage_end: Option<DateTime<Utc>>,
    pub generated_at: DateTime<Utc>,
    pub lag_seconds: u64,
    pub state: AnalyticsState,
    pub percentiles_approximate: bool,
    pub schema_version: u32,
    pub histogram_version: u32,
    /// 派生数据版本供缓存判断失效，不作为源事务提交顺序
    pub generation: u64,
}

/// 单次请求经 Provider 费用合同确认后可累加的 USD 情景金额
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageBillingProjection {
    pub standard_amount: Option<DecimalAmount>,
    pub no_cache_amount: Option<DecimalAmount>,
}

/// 时间桶内的情景费用，未知金额不改写为零
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageProjectedCostPoint {
    pub bucket_start: DateTime<Utc>,
    pub standard_amount: Option<DecimalAmount>,
    pub no_cache_amount: Option<DecimalAmount>,
}

/// 同一数据库快照内的完整洞察输入
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageInsightsObservation {
    pub overview: UsageOverview,
    pub trend: Vec<RequestMetricPoint>,
    pub costs: Vec<UsageProjectedCostPoint>,
}
