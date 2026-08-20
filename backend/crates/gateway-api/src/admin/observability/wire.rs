//! 响应 DTO 与固定 wire 形状。

use super::*;

/// 观测列表游标的稳定 wire 形状。
#[derive(Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CursorWire {
    pub observed_at: DateTime<Utc>,
    pub stable_id: String,
}

/// 观测列表响应数据。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageData<T> {
    pub items: Vec<T>,
    pub page: PageMeta,
    pub next_cursor: Option<String>,
}

/// Token 详情展示。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenDetailsView {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub image_input_tokens: Option<u64>,
    pub image_output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub input_tokens_display: String,
    pub output_tokens_display: String,
    pub cached_tokens_display: String,
    pub cache_write_tokens_display: String,
    pub reasoning_tokens_display: String,
    pub image_input_tokens_display: String,
    pub image_output_tokens_display: String,
    pub total_tokens_display: String,
}

/// 按货币展示的成本。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostView {
    pub currency: String,
    pub estimated_amount: String,
}

/// 成本覆盖状态计数。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostCoverageView {
    pub known: u64,
    pub partial: u64,
    pub unknown: u64,
    pub not_billable: u64,
}

/// Provider 受控价格规则生成的单次请求费用明细展示。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BillingView {
    pub input_amount_display: String,
    pub output_amount_display: String,
    pub cache_read_amount_display: String,
    pub cache_write_amount_display: String,
    pub standard_amount_display: String,
    pub total_amount_display: String,
    pub input_price_display: String,
    pub output_price_display: String,
    pub cache_read_price_display: String,
    pub cache_write_price_display: String,
    pub service_tier_display: String,
    pub multiplier_display: String,
}

/// 单条逻辑请求展示。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordView {
    pub id: String,
    pub request_id: String,
    pub client_api_key_id: Option<String>,
    pub routing_scope: String,
    pub routing_group_refs: Vec<String>,
    pub routing_group_names_snapshot: Vec<String>,
    pub kind: String,
    pub provider: Option<String>,
    pub authentication_kind: Option<String>,
    pub account_id: Option<String>,
    pub account_email: Option<String>,
    pub account_name: Option<String>,
    pub route: String,
    pub model: Option<String>,
    pub requested_model: Option<String>,
    pub upstream_model: Option<String>,
    pub service_tier: Option<String>,
    pub status_code: Option<i64>,
    pub transport: Option<String>,
    pub protocol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_status_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_status_code: Option<i64>,
    pub websocket_pool: Option<WebSocketPoolMetadataView>,
    pub image_generation_requested: bool,
    pub image_generation_succeeded: Option<bool>,
    pub latency_details: UsageLatencyDetailsView,
    pub attempt_index: Option<i64>,
    pub attempt_count: u64,
    pub response_id: Option<String>,
    pub upstream_request_id: Option<String>,
    pub latency_ms: Option<u64>,
    pub first_token_ms: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub image_input_tokens: Option<u64>,
    pub image_output_tokens: Option<u64>,
    pub message: String,
    /// Provider 安全观测；Core 字段由顶层字段提供，不再复制进 metadata。
    pub metadata: BTreeMap<String, Value>,
    pub created_at: DateTime<Utc>,
    pub created_at_display: String,
    pub client_ip: Option<String>,
    pub user_agent: Option<String>,
    pub reasoning_effort: Option<String>,
    pub reasoning_preset: Option<String>,
    pub compact: Option<bool>,
    pub request_kind: Option<String>,
    pub subagent_kind: Option<String>,
    pub token_details: TokenDetailsView,
    pub billing: Option<BillingView>,
    pub costs: Vec<CostView>,
    pub cost_coverage: CostCoverageView,
    pub first_token_latency_ms: Option<u64>,
    pub first_token_latency_ms_display: String,
    pub latency_ms_display: String,
    pub logical_outcome: String,
}

/// WebSocket 池决策的稳定形状；与 Provider 选择逻辑无关。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSocketPoolMetadataView {
    pub kind: String,
}

/// 逻辑请求在上游和输出阶段测得的时延事实。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLatencyDetailsView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport_decision_wait_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_connect_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_headers_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_event_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_reasoning_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_text_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai_processing_ms: Option<u64>,
}

/// 单次上游尝试展示。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageAttemptView {
    pub id: String,
    pub attempt_index: u32,
    pub trigger: String,
    pub provider: String,
    pub model: Option<String>,
    pub transport: String,
    pub send_state: String,
    pub outcome: String,
    pub downstream_committed: bool,
    pub status_code: Option<u16>,
    pub provider_error_code: Option<String>,
    pub failure_class: Option<String>,
    pub cost_estimate_status: String,
    pub estimated_cost_amount: Option<String>,
    pub estimated_cost_currency: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub first_token_ms: Option<u64>,
    pub latency_ms: Option<u64>,
    pub credential_name: Option<String>,
    pub account_id: Option<String>,
    pub account_name: Option<String>,
    pub account_email: Option<String>,
    pub authentication_kind: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// 逻辑请求详情与其尝试列表。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordDetailView {
    #[serde(flatten)]
    pub request: UsageRecordView,
    pub attempts: Vec<UsageAttemptView>,
    /// 尝试列表是否完整；best-effort 下恒为 false。
    pub attempts_complete: bool,
}

/// Dashboard 趋势数据。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrendData {
    pub kind: TrendKind,
    pub points: Vec<TrendPointView>,
    pub summary: Vec<TrendSummaryView>,
}

/// Dashboard 单个趋势桶。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrendPointView {
    pub time: String,
    pub bucket: DateTime<Utc>,
    pub label: String,
    pub requests: String,
    pub requests_value: u64,
    pub input_tokens: String,
    pub input_tokens_value: u64,
    pub output_tokens: String,
    pub output_tokens_value: u64,
    pub cached_tokens: String,
    pub cached_tokens_value: u64,
    pub cache_hit_rate_value: f64,
    pub tokens_value: u64,
    pub errors: String,
    pub errors_value: u64,
    pub latency: String,
    pub latency_value: Option<u64>,
    pub max_latency: String,
    pub max_latency_value: Option<u64>,
    pub min_latency: String,
    pub min_latency_value: Option<u64>,
    pub success_rate: String,
    pub success_rate_value: Option<f64>,
}

/// Dashboard 趋势摘要。
#[derive(Debug, Serialize)]
pub struct TrendSummaryView {
    pub label: String,
    pub value: String,
    pub ratio: Option<String>,
}

/// Dashboard 卡片集合。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardCardsView {
    pub credentials: DashboardCredentialsCardView,
    pub traffic: DashboardTrafficCardView,
    pub tokens: DashboardTokensCardView,
    pub cache: DashboardCacheCardView,
}

/// Dashboard 上游凭据卡片。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardCredentialsCardView {
    pub total: String,
    pub total_value: u64,
    pub available: String,
    pub available_value: u64,
    pub unavailable: String,
    pub unavailable_value: u64,
}

/// Dashboard 流量卡片。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardTrafficCardView {
    pub today_requests: String,
    pub today_requests_value: u64,
    pub yesterday_requests_value: u64,
    pub total_requests: String,
}

/// Dashboard token 卡片。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardTokensCardView {
    pub today_tokens: String,
    pub today_tokens_value: u64,
    pub yesterday_tokens_value: u64,
    pub total_tokens: String,
    pub total_billing_amount_usd: String,
}

/// Dashboard 缓存卡片。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardCacheCardView {
    pub today_hit_rate: String,
    pub today_hit_rate_value: Option<f64>,
    pub yesterday_hit_rate_value: Option<f64>,
    pub total_hit_rate: String,
    pub total_cached_tokens: String,
    pub average_first_token_latency_ms: String,
}

/// 旧 Dashboard 账号概览卡片所需的 Provider 安全投影。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardAccountUsageView {
    pub id: String,
    pub provider: String,
    pub authentication_kind: String,
    pub email: String,
    pub plan_type: Option<String>,
    pub tokens: String,
    pub request_count: u64,
    pub request_buckets: Vec<DashboardAccountRequestBucketView>,
    pub quota_used_percent: Option<f64>,
    pub last_used: String,
}

/// Dashboard 账号单小时请求数。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardAccountRequestBucketView {
    pub bucket_start: DateTime<Utc>,
    pub request_count: u64,
}

/// Provider 账号池的持久事实汇总。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardPoolSummaryView {
    pub total: u64,
    pub normal: u64,
    pub quota_exhausted: u64,
    pub rate_limited: u64,
    pub disabled: u64,
    pub error: u64,
}

/// 同 target 账号调度容量；Redis 未提供聚合事实时运行中槽位保持空值。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardCapacityInfoView {
    pub max_concurrent_per_account: u64,
    pub total_slots: u64,
    pub used_slots: Option<u64>,
    pub available_slots: Option<u64>,
}

/// 逻辑请求指标展示。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestMetricsView {
    pub request_count: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub cancelled_count: u64,
    pub incomplete_count: u64,
    pub caller_error_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
}

/// 上游尝试指标展示。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptMetricsView {
    pub attempt_count: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub cancelled_count: u64,
    pub incomplete_count: u64,
    pub rate_limited_count: u64,
    pub auth_failure_count: u64,
    pub provider5xx_count: u64,
    pub cost_coverage: CostCoverageView,
    pub costs: Vec<CostView>,
}

/// 健康时间线单点。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthTimelinePointView {
    pub time: String,
    pub status: String,
    pub reliability_display: String,
    pub success_requests: u64,
    pub failed_requests: u64,
    pub cancelled_requests: u64,
    pub incomplete_requests: u64,
    pub caller_error_requests: u64,
}

/// Dashboard 健康时间线。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthTimelineView {
    pub title: String,
    pub description: String,
    pub reliability_display: String,
    pub status: String,
    pub success_requests: u64,
    pub failed_requests: u64,
    pub cancelled_requests: u64,
    pub incomplete_requests: u64,
    pub caller_error_requests: u64,
    pub points: Vec<HealthTimelinePointView>,
}

/// Dashboard 展示的实际 Provider 上游请求身份。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardWireProfileView {
    pub provider: String,
    pub product: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    pub target: DashboardWireTargetView,
    pub user_agent: String,
    pub attributes: Vec<DashboardWireAttributeView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<DashboardDesktopReleaseView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardWireAttributeView {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardWireTargetView {
    pub os_type: String,
    pub os_version: String,
    pub arch: String,
    pub terminal: String,
}

/// 发布检查与启动画像分离；未检查时使用明确的 `unchecked` 状态。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardDesktopReleaseView {
    pub status: DashboardDesktopReleaseStatusView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_build: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Dashboard 发布检查的稳定 wire 状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DashboardDesktopReleaseStatusView {
    Unchecked,
    Aligned,
    ReviewRequired,
    CheckFailed,
}

impl From<domain::DesktopReleaseStatus> for DashboardDesktopReleaseStatusView {
    fn from(status: domain::DesktopReleaseStatus) -> Self {
        match status {
            domain::DesktopReleaseStatus::Unchecked => Self::Unchecked,
            domain::DesktopReleaseStatus::Current => Self::Aligned,
            domain::DesktopReleaseStatus::UpdateAvailable => Self::ReviewRequired,
            domain::DesktopReleaseStatus::Failed => Self::CheckFailed,
        }
    }
}

/// Dashboard 汇总响应数据。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardDataView {
    pub cards: DashboardCardsView,
    pub trend: TrendData,
    pub health_timeline: HealthTimelineView,
    pub wire_profiles: Vec<DashboardWireProfileView>,
    pub account_usage: Vec<DashboardAccountUsageView>,
    pub usage_records: Vec<UsageRecordView>,
    pub pool_summary: DashboardPoolSummaryView,
    pub capacity_info: DashboardCapacityInfoView,
    pub rotation_strategy: String,
}

/// 用量汇总响应数据。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummaryView {
    pub total_requests: String,
    pub input_tokens: String,
    pub output_tokens: String,
    pub cached_tokens: String,
    pub cache_write_tokens: String,
    pub total_tokens: String,
    pub average_latency_ms: String,
    pub logical_requests: RequestMetricsView,
    pub attempts: AttemptMetricsView,
}

/// 洞察健康趋势点。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewHealthPointView {
    pub bucket: DateTime<Utc>,
    pub label: String,
    pub success_requests: u64,
    pub failed_requests: u64,
    pub cancelled_requests: u64,
    pub incomplete_requests: u64,
    pub caller_error_requests: u64,
    pub error_rate: f64,
}

/// 洞察健康摘要。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewHealthView {
    pub total_requests: u64,
    pub success_requests: u64,
    pub failed_requests: u64,
    pub cancelled_requests: u64,
    pub incomplete_requests: u64,
    pub caller_error_requests: u64,
    pub success_rate: f64,
    pub request_change_rate: Option<f64>,
    pub success_rate_change: Option<f64>,
    pub points: Vec<OverviewHealthPointView>,
}

/// 洞察性能趋势点。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewPerformancePointView {
    pub bucket: DateTime<Utc>,
    pub label: String,
    pub latency_p50_ms: Option<f64>,
    pub latency_p95_ms: Option<f64>,
    pub latency_p99_ms: Option<f64>,
    pub first_token_p50_ms: Option<f64>,
    pub first_token_p95_ms: Option<f64>,
    pub first_token_p99_ms: Option<f64>,
}

/// 洞察性能摘要。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewPerformanceView {
    pub latency_p50_ms: Option<f64>,
    pub latency_p95_ms: Option<f64>,
    pub latency_p99_ms: Option<f64>,
    pub first_token_p50_ms: Option<f64>,
    pub first_token_p95_ms: Option<f64>,
    pub first_token_p99_ms: Option<f64>,
    pub latency_coverage: f64,
    pub first_token_coverage: f64,
    pub points: Vec<OverviewPerformancePointView>,
}

/// 洞察成本趋势点。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewCostPointView {
    pub bucket: DateTime<Utc>,
    pub label: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub total_tokens: u64,
    pub estimated_cost: Option<String>,
    pub standard_cost: Option<String>,
    pub cached_token_rate: f64,
    pub cache_hit_request_rate: Option<f64>,
}

/// 洞察成本摘要。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewCostView {
    pub estimated_cost: Option<String>,
    pub standard_cost: Option<String>,
    pub cost_per_request: Option<String>,
    pub tokens_per_request: f64,
    pub cached_token_rate: f64,
    pub cache_hit_request_rate: Option<f64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub total_tokens: u64,
    pub points: Vec<OverviewCostPointView>,
    pub costs: Vec<CostView>,
    pub coverage: CostCoverageView,
}

/// Provider 维度洞察摘要。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderOverviewView {
    pub provider: String,
    pub request_count: u64,
    pub attempt_count: u64,
    pub failure_count: u64,
    pub total_tokens: u64,
}

/// 用量洞察总响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageInsightsOverviewView {
    pub granularity: String,
    pub health: OverviewHealthView,
    pub performance: OverviewPerformanceView,
    pub cost: OverviewCostView,
    pub attempts: AttemptMetricsView,
    pub providers: Vec<ProviderOverviewView>,
}

/// 诊断聚合项目。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticItemView {
    pub key: String,
    pub name: String,
    pub request_count: u64,
    pub success_count: u64,
    pub error_count: u64,
    pub error_rate: f64,
    pub request_share: f64,
    pub average_latency_ms: Option<u64>,
    pub latency_p95_ms: Option<u64>,
    pub estimated_cost: Option<String>,
    pub attempt_count: u64,
    pub total_tokens: u64,
}

/// 诊断聚合响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsView {
    pub dimension: String,
    pub items: Vec<DiagnosticItemView>,
}

/// 运维错误项目。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsErrorView {
    pub id: String,
    pub request_id: Option<String>,
    pub client_api_key_id: Option<String>,
    pub kind: String,
    pub provider: Option<String>,
    pub authentication_kind: Option<String>,
    pub account_id: Option<String>,
    pub route: String,
    pub model: Option<String>,
    pub client_status_code: Option<i64>,
    pub upstream_status_code: Option<i64>,
    pub transport: Option<String>,
    pub attempt_index: Option<u32>,
    pub failure_class: String,
    pub response_id: Option<String>,
    pub upstream_request_id: Option<String>,
    pub latency_ms: Option<u64>,
    pub message: String,
    pub metadata: OpsErrorMetadataView,
    pub created_at: DateTime<Utc>,
    pub created_at_display: String,
}

/// 运维错误安全元数据；只保留可查询的标识，不回显秘密材料。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsErrorMetadataView {
    pub source: String,
    pub component: String,
    pub attempt_id: Option<String>,
    pub account_label: Option<String>,
}
