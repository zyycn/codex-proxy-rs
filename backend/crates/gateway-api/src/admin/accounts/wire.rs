//! 账号管理请求、响应与查询 wire contract。

use super::*;

/// 账号列表查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListQuery {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub provider: Option<String>,
    pub group_id: Option<String>,
    pub search: Option<String>,
    pub status: Option<String>,
    pub sort_by: Option<String>,
    pub sort_direction: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchUpdateAccountsRequest {
    pub account_ids: Vec<String>,
    pub enabled: bool,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub concurrency_limit: Option<u64>,
    pub weight: u64,
    pub group_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchUpdatedAccountsData {
    account_ids: Vec<String>,
    config_revision: u64,
}

impl From<AccountsUpdateResult> for BatchUpdatedAccountsData {
    fn from(result: AccountsUpdateResult) -> Self {
        Self {
            account_ids: result
                .account_ids
                .into_iter()
                .map(|id| id.to_string())
                .collect(),
            config_revision: result.config_revision.get(),
        }
    }
}

impl BatchUpdateAccountsRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        if self.account_ids.is_empty()
            || self.account_ids.len() > MAX_ACCOUNT_GROUP_BATCH
            || self
                .account_ids
                .iter()
                .any(|id| require_account_id(id, "accountIds").is_err())
            || self.account_ids.iter().collect::<BTreeSet<_>>().len() != self.account_ids.len()
        {
            return Err(WireValidationError::new("accountIds"));
        }
        validate_wire_group_ids(&self.group_ids)?;
        parse_concurrency_limit(self.concurrency_limit)?;
        parse_account_weight(self.weight)?;
        Ok(())
    }

    pub(super) fn into_command(self) -> Result<BatchUpdateAccounts, WireValidationError> {
        self.validate()?;
        Ok(BatchUpdateAccounts {
            account_ids: self.account_ids,
            enabled: self.enabled,
            concurrency_limit: parse_concurrency_limit(self.concurrency_limit)?,
            weight: parse_account_weight(self.weight)?,
            group_ids: validate_wire_group_ids(&self.group_ids)?,
        })
    }
}

impl ListQuery {
    /// 解析并校验全部 wire 字段，生成 Admin 查询命令。
    pub fn validate(self) -> Result<AccountListQuery, WireValidationError> {
        let page = self.page.unwrap_or(1);
        if page == 0 {
            return Err(WireValidationError::new("page"));
        }
        let page_size = self.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
        if page_size == 0 || page_size > MAX_PAGE_SIZE {
            return Err(WireValidationError::new("pageSize"));
        }
        let provider_kind = parse_provider(self.provider.as_deref().unwrap_or("all"))?;
        let group_filter = match self.group_id.as_deref().map(str::trim) {
            None | Some("") => None,
            Some("ungrouped") => Some(AccountGroupFilter::Ungrouped),
            Some(value) => Some(AccountGroupFilter::Group(
                AccountGroupId::new(value.to_owned())
                    .map_err(|_| WireValidationError::new("groupId"))?,
            )),
        };
        let search = self
            .search
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if search.as_deref().is_some_and(|value| {
            value.len() > MAX_SEARCH_BYTES || value.chars().any(char::is_control)
        }) {
            return Err(WireValidationError::new("search"));
        }
        let status = match self.status.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(value) => Some(
                DomainAccountStatus::parse(&value.to_ascii_lowercase())
                    .ok_or_else(|| WireValidationError::new("status"))?,
            ),
        };
        let sort = match (self.sort_by.as_deref(), self.sort_direction.as_deref()) {
            (None, None) => None,
            (Some(field), Some(direction)) => Some(AccountSort {
                field: parse_sort_field(field).ok_or_else(|| WireValidationError::new("sortBy"))?,
                direction: parse_sort_direction(direction)
                    .ok_or_else(|| WireValidationError::new("sortDirection"))?,
            }),
            _ => return Err(WireValidationError::new("sort")),
        };
        Ok(AccountListQuery {
            page,
            page_size: PageSize::new(
                u16::try_from(page_size).map_err(|_| WireValidationError::new("pageSize"))?,
            )
            .map_err(|_| WireValidationError::new("pageSize"))?,
            provider_kind,
            group_filter,
            search,
            status,
            sort,
        })
    }
}

fn parse_provider(value: &str) -> Result<Option<ProviderKind>, WireValidationError> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    ProviderKind::new(value.to_owned())
        .map(Some)
        .map_err(|_| WireValidationError::new("provider"))
}

fn parse_sort_field(value: &str) -> Option<AccountSortField> {
    match value.trim() {
        "email" => Some(AccountSortField::Email),
        "status" => Some(AccountSortField::Status),
        "planType" => Some(AccountSortField::PlanType),
        "usage" => Some(AccountSortField::Usage),
        "lastUsedAt" => Some(AccountSortField::LastUsedAt),
        "expiresAt" => Some(AccountSortField::ExpiresAt),
        _ => None,
    }
}

fn parse_sort_direction(value: &str) -> Option<SortDirection> {
    match value.trim().to_ascii_lowercase().as_str() {
        "asc" => Some(SortDirection::Asc),
        "desc" => Some(SortDirection::Desc),
        _ => None,
    }
}

/// 账号列表响应数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountPageData {
    pub items: Vec<AccountView>,
    pub page: PageMeta,
    pub summary: AccountSummaryView,
}

/// 账号概览计数。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSummaryView {
    pub total: u64,
    pub normal: u64,
    pub quota_exhausted: u64,
    pub rate_limited: u64,
    pub disabled: u64,
    pub error: u64,
}

/// 一条安全账号视图。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub groups: Vec<AccountGroupRefView>,
    pub resource_ref: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub authentication_kind: String,
    pub has_refresh_token: bool,
    pub status: String,
    /// `status == "error"` 时的具体原因；其余状态为 `null`。
    pub error_reason: Option<String>,
    /// 最近一次失败的上游错误描述；仅错误状态存在。
    pub error_message: Option<String>,
    pub enabled: bool,
    pub concurrency_limit: Option<u32>,
    pub weight: u16,
    pub access_token_expires_at: Option<String>,
    pub access_token_expires_at_display: Option<String>,
    pub refresh_token_expires_at: Option<String>,
    pub next_refresh_at: Option<String>,
    pub added_at: String,
    pub added_at_display: String,
    pub updated_at: String,
    pub updated_at_display: String,
    pub quota: AccountQuotaView,
    pub usage: AccountUsageView,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountGroupRefView {
    pub id: String,
    pub name: String,
    pub color: String,
    pub enabled: bool,
}

/// Provider quota 安全视图。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQuotaView {
    pub refreshed_at_display: String,
    pub limit_reached: bool,
    /// 429 临时限流（Redis 冷却）到期时间展示；非限流中为 `null`。
    pub rate_limited_until: Option<String>,
    pub windows: Vec<AccountQuotaWindowView>,
}

/// 一个 quota 时间窗口。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQuotaWindowView {
    pub key: String,
    pub group: String,
    pub limit_id: Option<String>,
    pub limit_name: Option<String>,
    pub role: Option<String>,
    pub window_seconds: Option<u64>,
    pub label_display: String,
    pub window_label_display: String,
    pub used_percent: Option<f64>,
    pub used_percent_display: String,
    pub limit_reached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_usage: Option<serde_json::Value>,
    pub reset_at_display: String,
}

/// 账号观测用量。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsageView {
    pub request_count: Option<u64>,
    pub request_count_display: String,
    pub input_tokens: Option<u64>,
    pub input_tokens_display: String,
    pub output_tokens: Option<u64>,
    pub output_tokens_display: String,
    pub cached_tokens: Option<u64>,
    pub cached_tokens_display: String,
    pub image_input_tokens: Option<u64>,
    pub image_input_tokens_display: String,
    pub image_output_tokens: Option<u64>,
    pub image_output_tokens_display: String,
    pub image_request_count: Option<u64>,
    pub image_request_count_display: String,
    pub image_request_failed_count: Option<u64>,
    pub image_request_failed_count_display: String,
    pub total_tokens: Option<u64>,
    pub total_tokens_display: String,
    pub created_tokens: Option<u64>,
    pub created_tokens_display: String,
    pub read_tokens: Option<u64>,
    pub read_tokens_display: String,
    pub last_used_at: Option<String>,
    pub last_used_at_display: String,
    pub cost_estimate_status: String,
    pub known_cost_count: Option<u64>,
    pub partial_cost_count: Option<u64>,
    pub unknown_cost_count: Option<u64>,
    pub costs: Vec<CurrencyCostView>,
    pub models: Vec<ModelUsageView>,
}

/// 凭据在单个上游模型上的观测用量。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsageView {
    pub model: String,
    pub request_count: u64,
    pub request_count_display: String,
    pub success_rate: Option<f64>,
    pub success_rate_display: String,
    pub input_tokens: Option<u64>,
    pub input_tokens_display: String,
    pub output_tokens: Option<u64>,
    pub output_tokens_display: String,
    pub cached_tokens: Option<u64>,
    pub cached_tokens_display: String,
    pub image_input_tokens: Option<u64>,
    pub image_input_tokens_display: String,
    pub image_output_tokens: Option<u64>,
    pub image_output_tokens_display: String,
    pub image_request_count: u64,
    pub image_request_count_display: String,
    pub image_request_failed_count: u64,
    pub image_request_failed_count_display: String,
    pub total_tokens: Option<u64>,
    pub total_tokens_display: String,
    pub billing_amount_usd: Option<String>,
    pub billing_amount_usd_display: String,
    pub cost_estimate_status: String,
    pub known_cost_count: u64,
    pub partial_cost_count: u64,
    pub unknown_cost_count: u64,
    pub costs: Vec<CurrencyCostView>,
    pub last_used_at: String,
    pub last_used_at_display: String,
}

/// 单一货币的可查询成本。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrencyCostView {
    pub currency: String,
    pub estimated_amount: String,
    pub estimated_amount_display: String,
}

/// 账号详情类 GET 的固定 ID query。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountIdQuery {
    pub account_id: String,
}

impl AccountIdQuery {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_account_id(&self.account_id, "accountId")
    }

    pub(super) fn into_id(self) -> Result<ProviderAccountId, WireValidationError> {
        self.validate()?;
        ProviderAccountId::new(self.account_id).map_err(|_| WireValidationError::new("accountId"))
    }
}

/// 头像 GET 的固定 query；`version` 只参与浏览器缓存键，不进入上游请求。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountProfileAvatarQuery {
    pub account_id: String,
    pub version: Option<String>,
}

impl AccountProfileAvatarQuery {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_account_id(&self.account_id, "accountId")?;
        if self.version.as_deref().is_some_and(|version| {
            version.is_empty()
                || version.len() > MAX_AVATAR_VERSION_BYTES
                || !version.bytes().all(|byte| byte.is_ascii_alphanumeric())
        }) {
            return Err(WireValidationError::new("version"));
        }
        Ok(())
    }

    pub(super) fn into_id(self) -> Result<ProviderAccountId, WireValidationError> {
        self.validate()?;
        ProviderAccountId::new(self.account_id).map_err(|_| WireValidationError::new("accountId"))
    }
}

/// 敏感导出的固定 query；IDs 使用逗号分隔，禁止隐式导出全部账号。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountExportQuery {
    pub account_ids: String,
    pub confirm: String,
}

impl AccountExportQuery {
    pub fn into_ids(self) -> Result<Vec<ProviderAccountId>, WireValidationError> {
        if self.confirm != "export_sensitive_accounts" {
            return Err(WireValidationError::new("confirm"));
        }
        let ids = self
            .account_ids
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if ids.is_empty()
            || ids.len() > 200
            || ids
                .iter()
                .any(|id| require_account_id(id, "accountIds").is_err())
        {
            return Err(WireValidationError::new("accountIds"));
        }
        let unique = ids.iter().collect::<BTreeSet<_>>();
        if unique.len() != ids.len() {
            return Err(WireValidationError::new("accountIds"));
        }
        ids.into_iter()
            .map(|id| {
                ProviderAccountId::new(id).map_err(|_| WireValidationError::new("accountIds"))
            })
            .collect()
    }
}

/// 账号运行期动作。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountActionRequest {
    pub account_id: String,
}

impl AccountActionRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_account_id(&self.account_id, "accountId")
    }

    pub(super) fn into_id(self) -> Result<ProviderAccountId, WireValidationError> {
        self.validate()?;
        ProviderAccountId::new(self.account_id).map_err(|_| WireValidationError::new("accountId"))
    }
}

/// 主动额度重置卡消费请求。幂等键由 UI 生成并在不确定重试时复用，与官方一致。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountResetCreditConsumeRequest {
    pub account_id: String,
    #[serde(default)]
    pub credit_id: Option<String>,
    pub redeem_request_id: String,
}

impl AccountResetCreditConsumeRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_account_id(&self.account_id, "accountId")?;
        if self.credit_id.as_deref().is_some_and(|value| {
            let value = value.trim();
            value.is_empty() || value.len() > 512 || value.chars().any(char::is_control)
        }) {
            return Err(WireValidationError::new("creditId"));
        }
        let value = self.redeem_request_id.trim();
        let redeem_request_id =
            Uuid::parse_str(value).map_err(|_| WireValidationError::new("redeemRequestId"))?;
        if redeem_request_id.get_version() != Some(Version::Random)
            || redeem_request_id.hyphenated().to_string() != value
        {
            return Err(WireValidationError::new("redeemRequestId"));
        }
        Ok(())
    }

    pub(super) fn into_command(self) -> Result<ConsumeProviderResetCredit, WireValidationError> {
        self.validate()?;
        Ok(ConsumeProviderResetCredit {
            account_id: ProviderAccountId::new(self.account_id)
                .map_err(|_| WireValidationError::new("accountId"))?,
            credit_id: self.credit_id.map(|value| value.trim().to_owned()),
            redeem_request_id: Uuid::parse_str(&self.redeem_request_id)
                .map_err(|_| WireValidationError::new("redeemRequestId"))?,
        })
    }
}

/// 手工 OAuth 刷新会变更持久 credential。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountRefreshRequest {
    pub account_id: String,
}

impl AccountRefreshRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_account_id(&self.account_id, "accountId")
    }

    pub(super) fn into_command(self) -> Result<ProviderAccountId, WireValidationError> {
        self.validate()?;
        ProviderAccountId::new(self.account_id).map_err(|_| WireValidationError::new("accountId"))
    }
}

/// 连接测试 query；测试仍经唯一 Core/Provider 模型请求路径执行。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountTestQuery {
    pub account_id: String,
    pub model_id: String,
}

impl AccountTestQuery {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_account_id(&self.account_id, "accountId")?;
        if self.model_id.trim().is_empty() || self.model_id.chars().any(char::is_control) {
            return Err(WireValidationError::new("modelId"));
        }
        Ok(())
    }

    pub(super) fn into_command(
        self,
    ) -> Result<(ProviderAccountId, UpstreamModelId), WireValidationError> {
        self.validate()?;
        Ok((
            ProviderAccountId::new(self.account_id)
                .map_err(|_| WireValidationError::new("accountId"))?,
            UpstreamModelId::new(self.model_id).map_err(|_| WireValidationError::new("modelId"))?,
        ))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountModelView {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountModelsData {
    pub models: Vec<AccountModelView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRefreshData {
    pub account: AccountView,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountQuotaData {
    pub account: AccountView,
}

/// Provider 官方个人资料统计响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountProfileStatisticsData {
    pub display_name: Option<String>,
    pub username: Option<String>,
    pub image_url: Option<String>,
    pub has_stats_error: bool,
    pub summary: AccountProfileStatisticsSummaryView,
    pub daily_usage: Option<Vec<AccountProfileDailyUsageView>>,
    pub activity_insights: AccountProfileActivityInsightsView,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountProfileStatisticsSummaryView {
    pub total_text_tokens: Option<u64>,
    pub peak_tokens: Option<u64>,
    pub longest_task_duration_ms: Option<u64>,
    pub current_streak_days: Option<u64>,
    pub longest_streak_days: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountProfileDailyUsageView {
    pub date: String,
    pub tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountProfileActivityInsightsView {
    pub fast_mode_percent: Option<f64>,
    pub reasoning_effort: Option<String>,
    pub reasoning_effort_percent: Option<f64>,
    pub skills_explored: Option<u64>,
    pub total_skills_used: Option<u64>,
    pub total_threads: Option<u64>,
    pub invocations: Option<Vec<AccountProfileInvocationView>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountProfileInvocationView {
    #[serde(rename = "type")]
    pub invocation_type: String,
    pub plugin_id: Option<String>,
    pub plugin_name: Option<String>,
    pub skill_id: Option<String>,
    pub skill_name: Option<String>,
    pub usage_count: Option<u64>,
}

impl From<ProviderProfileStatistics> for AccountProfileStatisticsData {
    fn from(statistics: ProviderProfileStatistics) -> Self {
        Self {
            display_name: statistics.display_name,
            username: statistics.username,
            image_url: statistics.image_url,
            has_stats_error: statistics.has_stats_error,
            summary: profile_statistics_summary_view(statistics.summary),
            daily_usage: statistics
                .daily_usage
                .map(|daily| daily.into_iter().map(profile_daily_usage_view).collect()),
            activity_insights: profile_activity_insights_view(statistics.activity_insights),
        }
    }
}

const fn profile_statistics_summary_view(
    summary: ProviderProfileStatisticsSummary,
) -> AccountProfileStatisticsSummaryView {
    AccountProfileStatisticsSummaryView {
        total_text_tokens: summary.total_text_tokens,
        peak_tokens: summary.peak_tokens,
        longest_task_duration_ms: summary.longest_task_duration_ms,
        current_streak_days: summary.current_streak_days,
        longest_streak_days: summary.longest_streak_days,
    }
}

fn profile_daily_usage_view(daily: ProviderProfileDailyUsage) -> AccountProfileDailyUsageView {
    AccountProfileDailyUsageView {
        date: daily.date.to_string(),
        tokens: daily.tokens,
    }
}

fn profile_activity_insights_view(
    insights: ProviderProfileActivityInsights,
) -> AccountProfileActivityInsightsView {
    AccountProfileActivityInsightsView {
        fast_mode_percent: insights.fast_mode_percent,
        reasoning_effort: insights.reasoning_effort,
        reasoning_effort_percent: insights.reasoning_effort_percent,
        skills_explored: insights.skills_explored,
        total_skills_used: insights.total_skills_used,
        total_threads: insights.total_threads,
        invocations: insights.invocations.map(|invocations| {
            invocations
                .into_iter()
                .map(profile_invocation_view)
                .collect()
        }),
    }
}

fn profile_invocation_view(invocation: ProviderProfileInvocation) -> AccountProfileInvocationView {
    AccountProfileInvocationView {
        invocation_type: invocation.invocation_type,
        plugin_id: invocation.plugin_id,
        plugin_name: invocation.plugin_name,
        skill_id: invocation.skill_id,
        skill_name: invocation.skill_name,
        usage_count: invocation.usage_count,
    }
}

/// 主动额度重置卡列表响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountResetCreditsData {
    pub available_count: u64,
    pub credits: Vec<AccountResetCreditView>,
}

/// 一张安全主动额度重置卡视图。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountResetCreditView {
    pub id: String,
    pub status: Option<String>,
    pub title: Option<String>,
    pub expires_at: Option<String>,
    pub reset_type: Option<String>,
}

/// 主动额度重置卡消费响应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountResetCreditResultData {
    pub code: String,
    pub credit: Option<AccountResetCreditView>,
}

impl From<ProviderResetCredit> for AccountResetCreditView {
    fn from(credit: ProviderResetCredit) -> Self {
        Self {
            id: credit.id,
            status: credit.status,
            title: credit.title,
            expires_at: credit.expires_at.map(|value| value.to_rfc3339()),
            reset_type: credit.reset_type,
        }
    }
}

impl From<ProviderResetCredits> for AccountResetCreditsData {
    fn from(credits: ProviderResetCredits) -> Self {
        Self {
            available_count: credits.available_count,
            credits: credits
                .credits
                .into_iter()
                .map(AccountResetCreditView::from)
                .collect(),
        }
    }
}

impl From<ProviderResetCreditResult> for AccountResetCreditResultData {
    fn from(result: ProviderResetCreditResult) -> Self {
        Self {
            code: result.code,
            credit: result.credit.map(AccountResetCreditView::from),
        }
    }
}

/// Provider-owned 明文导出文档；Debug 永远不输出内部 JSON。
#[derive(Serialize)]
#[serde(transparent)]
pub struct AccountExportData(Value);

impl AccountExportData {
    #[must_use]
    pub const fn new(value: Value) -> Self {
        Self(value)
    }

    pub(super) fn from_result(bundle: AccountExportBundle) -> Self {
        let documents = bundle
            .documents
            .into_iter()
            .map(|document| {
                serde_json::json!({
                    "provider": document.provider_kind.to_string(),
                    "document": provider_document_value(document.document),
                })
            })
            .collect::<Vec<_>>();
        Self::new(serde_json::json!({
            "exportedAt": bundle.exported_at.to_rfc3339(),
            "documents": documents,
        }))
    }
}

impl fmt::Debug for AccountExportData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccountExportData(<redacted>)")
    }
}

pub struct AccountConnectionTestEvent {
    pub data: Value,
}

impl From<DomainConnectionTestEvent> for AccountConnectionTestEvent {
    fn from(event: DomainConnectionTestEvent) -> Self {
        let data = match event {
            DomainConnectionTestEvent::Started { model } => serde_json::json!({
                "type": "test_start",
                "model": model,
                "text": "正在连接上游 Responses"
            }),
            DomainConnectionTestEvent::Request {
                model,
                input_text,
                stream,
                store,
            } => serde_json::json!({
                "type": "request",
                "payload": {
                    "model": model,
                    "input": [{
                        "role": "user",
                        "content": [{ "type": "input_text", "text": input_text }]
                    }],
                    "stream": stream,
                    "store": store
                }
            }),
            DomainConnectionTestEvent::Content { text } => {
                serde_json::json!({ "type": "content", "text": text })
            }
            DomainConnectionTestEvent::Completed => serde_json::json!({
                "type": "test_complete",
                "success": true
            }),
            DomainConnectionTestEvent::Failed {
                source,
                gateway_error_code,
                send_state,
                message,
                provider_error_code,
                provider_error_type,
                upstream_status,
                upstream_content_type,
                upstream_body,
            } => serde_json::json!({
                "type": "error",
                "source": source.as_str(),
                "gatewayErrorCode": gateway_error_code.as_str(),
                "sendState": send_state.map(gateway_core::upstream::UpstreamSendState::as_str),
                "error": message,
                "providerErrorCode": provider_error_code,
                "providerErrorType": provider_error_type,
                "upstreamStatus": upstream_status,
                "upstreamContentType": upstream_content_type,
                "upstreamBody": upstream_body
            }),
        };
        Self { data }
    }
}
