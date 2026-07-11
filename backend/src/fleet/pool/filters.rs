//! 账号获取过滤条件、池配置与选择结果类型。

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountPoolOptions {
    /// 单账号允许的最大并发请求数。
    pub max_concurrent_per_account: usize,
    /// 在途槽位未释放时的过期时间。
    pub stale_slot_ttl: Duration,
    /// 账号选择轮转策略。
    pub rotation_strategy: RotationStrategy,
    /// 是否跳过已标记配额耗尽的账号。
    pub skip_quota_limited: bool,
    /// 订阅层级优先级，越靠前优先级越高。
    pub tier_priority: Vec<String>,
    /// 模型到允许订阅计划的映射。
    pub model_plan_allowlist: BTreeMap<String, Vec<String>>,
    /// 已成功拉取过模型列表的订阅计划。
    pub fetched_model_plan_types: BTreeSet<String>,
}

impl Default for AccountPoolOptions {
    fn default() -> Self {
        Self {
            max_concurrent_per_account: 3,
            stale_slot_ttl: Duration::minutes(5),
            rotation_strategy: RotationStrategy::Smart,
            skip_quota_limited: true,
            tier_priority: Vec::new(),
            model_plan_allowlist: BTreeMap::new(),
            fetched_model_plan_types: BTreeSet::new(),
        }
    }
}

/// 账号获取请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountAcquireRequest {
    /// 本次请求使用的模型。
    pub model: String,
    /// 本次请求需要排除的账号 ID。
    pub exclude_account_ids: Vec<String>,
    /// 本次请求必须使用的账号 ID，用于同请求内的同账号重试。
    pub required_account_id: Option<String>,
    /// 会话亲和性建议的优先账号 ID。
    pub preferred_account_id: Option<String>,
    /// 本次调度使用的当前时间。
    pub now: DateTime<Utc>,
}

impl AccountAcquireRequest {
    /// 构造账号获取请求。
    pub fn new(model: impl Into<String>, now: DateTime<Utc>) -> Self {
        Self {
            model: model.into(),
            exclude_account_ids: Vec::new(),
            required_account_id: None,
            preferred_account_id: None,
            now,
        }
    }

    /// 设置需要排除的账号 ID。
    pub fn with_exclude_account_ids(
        mut self,
        account_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.exclude_account_ids = account_ids.into_iter().map(Into::into).collect();
        self
    }

    /// 设置会话亲和性建议的优先账号 ID。
    pub fn with_preferred_account_id(mut self, account_id: impl Into<String>) -> Self {
        self.preferred_account_id = Some(account_id.into());
        self
    }

    /// 设置本次调度必须使用的账号 ID。
    pub fn with_required_account_id(mut self, account_id: impl Into<String>) -> Self {
        self.required_account_id = Some(account_id.into());
        self
    }
}

/// 成功获取到的账号及调度元数据。
#[derive(Debug, Clone)]
pub struct AcquiredAccount {
    /// 被选中的账号快照。
    pub account: Account,
    /// 同一账号上一个在途槽位的创建时间。
    pub previous_slot_at: Option<DateTime<Utc>>,
    pub(super) slot_id: uuid::Uuid,
}

impl std::ops::Deref for AcquiredAccount {
    type Target = Account;

    fn deref(&self) -> &Self::Target {
        &self.account
    }
}

/// 模型刷新时每个订阅计划选中的账号。
#[derive(Debug, Clone)]
pub struct DistinctPlanAccount {
    /// 订阅计划类型。
    pub plan_type: String,
    /// 被选中的账号快照。
    pub account: Account,
    pub(super) slot_id: uuid::Uuid,
}
