//! 快照/窗口解析、聚合、O(1) 滚动与调度信号。
//!
//! Provider raw quota JSON 的唯一解析者：产出 [`CodexAccountQuotaSnapshot`]
//! 供服务编排层消费；`limit_reached` 是快照级事实，不能只从子窗口反推。

use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use gateway_core::engine::credential::{
    AccountQuotaSignals, CredentialRevision, ProviderAccountId, QuotaObservation,
};
use serde_json::{Map, Value};

use super::{CodexCredentialQuotaError, QUOTA_SCHEDULING_TTL};

const CORE_PRIMARY_WINDOW_KEY: &str = "core-primary";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexQuotaFact {
    remaining_percent: Option<u8>,
    resets_at: Option<DateTime<Utc>>,
    exhausted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexQuotaWindowKind {
    ShortTerm,
    Weekly,
    Monthly,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexQuotaWindowRole {
    Primary,
    Secondary,
    Monthly,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexQuotaWindow {
    key: String,
    source: String,
    /// 上游提供的限流桶名称（`metered_feature`/`limit_name`/`limitId`）；用于非核心桶的展示前缀。
    limit_name: Option<String>,
    kind: CodexQuotaWindowKind,
    role: CodexQuotaWindowRole,
    window_seconds: Option<u64>,
    used_percent: Option<f64>,
    reset_at: Option<DateTime<Utc>>,
    limit_reached: bool,
}

impl CodexQuotaWindow {
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub fn limit_name(&self) -> Option<&str> {
        self.limit_name.as_deref()
    }

    #[must_use]
    pub const fn kind(&self) -> CodexQuotaWindowKind {
        self.kind
    }

    #[must_use]
    pub const fn role(&self) -> CodexQuotaWindowRole {
        self.role
    }

    #[must_use]
    pub const fn window_seconds(&self) -> Option<u64> {
        self.window_seconds
    }

    #[must_use]
    pub const fn used_percent(&self) -> Option<f64> {
        self.used_percent
    }

    #[must_use]
    pub const fn reset_at(&self) -> Option<DateTime<Utc>> {
        self.reset_at
    }

    /// 该窗口是否已触顶（`limit_reached` 或 `used_percent >= 100`）。
    #[must_use]
    pub const fn limit_reached(&self) -> bool {
        self.limit_reached
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexAccountQuotaSnapshot {
    account_id: ProviderAccountId,
    credential_revision: CredentialRevision,
    observed_at: SystemTime,
    fact: CodexQuotaFact,
    authoritative_core_primary_exhausted: bool,
    windows: Vec<CodexQuotaWindow>,
}

impl CodexAccountQuotaSnapshot {
    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn credential_revision(&self) -> CredentialRevision {
        self.credential_revision
    }

    #[must_use]
    pub const fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    #[must_use]
    pub const fn fact(&self) -> CodexQuotaFact {
        self.fact
    }

    #[must_use]
    pub fn windows(&self) -> &[CodexQuotaWindow] {
        &self.windows
    }

    #[must_use]
    pub(crate) fn core_primary_window(&self) -> Option<&CodexQuotaWindow> {
        self.windows
            .iter()
            .find(|window| window.key() == CORE_PRIMARY_WINDOW_KEY)
    }

    /// 上游成功 usage 明确拒绝请求，且 canonical core primary 已确认触顶。
    #[must_use]
    pub(crate) const fn authoritative_core_primary_exhausted(&self) -> bool {
        self.authoritative_core_primary_exhausted
    }

    /// 投影滚动已过期窗口：`reset_at` 已过的窗口 `used_percent=0`、
    /// `limit_reached=false`、`reset_at` 滚到下一个周期（reset 到期自动归零）。
    /// 只作用于展示/调度投影，不写回持久化 raw JSON。
    ///
    /// O(1) 计算：`next_reset = reset + (floor((now - reset) / window) + 1) * window`，
    /// 不做逐周期线性循环。
    #[must_use]
    pub fn roll_expired_windows(mut self, now: SystemTime) -> Self {
        for window in &mut self.windows {
            let Some(reset_at) = window.reset_at else {
                continue;
            };
            let reset = SystemTime::from(reset_at);
            if reset > now {
                continue;
            }
            window.used_percent = Some(0.0);
            window.limit_reached = false;
            let Some(window_seconds) = window.window_seconds.filter(|seconds| *seconds > 0) else {
                // 没有合法窗口时长：清除已过期的 reset，等待下一次权威观测。
                window.reset_at = None;
                continue;
            };
            let elapsed = now
                .duration_since(reset)
                .unwrap_or(Duration::ZERO)
                .as_secs();
            let periods = elapsed / window_seconds + 1;
            let Some(after) =
                reset.checked_add(Duration::from_secs(periods.saturating_mul(window_seconds)))
            else {
                window.reset_at = None;
                continue;
            };
            window.reset_at = Some(DateTime::<Utc>::from(after));
        }
        self
    }
}

impl CodexQuotaFact {
    #[must_use]
    pub const fn remaining_percent(&self) -> Option<u8> {
        self.remaining_percent
    }

    #[must_use]
    pub const fn resets_at(&self) -> Option<DateTime<Utc>> {
        self.resets_at
    }

    #[must_use]
    pub const fn exhausted(&self) -> bool {
        self.exhausted
    }
}

pub fn parse_codex_quota_usage(usage: &Value) -> Result<CodexQuotaFact, CodexCredentialQuotaError> {
    let object = usage
        .as_object()
        .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
    let mut aggregate = QuotaAggregate::default();
    for limit in canonical_rate_limits(object)? {
        aggregate.observe_rate_limit(limit.rate_limit)?;
    }
    // spend_control 只提供 exhaustion 信号（`reached`），不生成窗口、
    // 不参与剩余量聚合（`individual_limit` 等字段无窗口语义）。
    if let Some(spend_control) = object.get("spend_control") {
        aggregate.observe_exhaustion_object(spend_control, "reached")?;
    }
    if let Some(credits) = object.get("credits") {
        aggregate.observe_exhaustion_object(credits, "overage_limit_reached")?;
    }
    if !aggregate.recognized {
        return Err(CodexCredentialQuotaError::InvalidCredentialData);
    }
    Ok(CodexQuotaFact {
        remaining_percent: aggregate.remaining_percent,
        resets_at: aggregate.resets_at,
        exhausted: aggregate.exhausted,
    })
}

pub(crate) fn quota_snapshot_from_observation(
    observation: &QuotaObservation,
) -> Option<CodexAccountQuotaSnapshot> {
    let observed_at = observation.observed_at?;
    let quota = observation.quota.as_ref()?;
    parse_account_quota_snapshot(
        observation.account_id.clone(),
        observation.expected_revision,
        observed_at,
        &Value::Object(quota.expose_to_provider().clone()),
    )
    .ok()
}

/// 调度投影缓存 TTL：`min(常规 TTL, 最近 active reset - now)`。
/// reset 到期时缓存自然失效，不固定多等一个常规 TTL 周期。
pub(crate) fn quota_projection_ttl(snapshot: &CodexAccountQuotaSnapshot) -> Option<Duration> {
    let age = SystemTime::now()
        .duration_since(snapshot.observed_at())
        .unwrap_or(Duration::ZERO);
    let normal_ttl = QUOTA_SCHEDULING_TTL.checked_sub(age)?;
    let now = SystemTime::now();
    let reset_ttl = snapshot
        .windows()
        .iter()
        .filter_map(|window| window.reset_at())
        .map(SystemTime::from)
        .filter(|reset| *reset > now)
        .map(|reset| reset.duration_since(now).unwrap_or(Duration::ZERO))
        .min();
    let ttl = match reset_ttl {
        Some(reset_ttl) if reset_ttl < normal_ttl => reset_ttl,
        _ => normal_ttl,
    };
    (!ttl.is_zero()).then_some(ttl)
}

/// 从快照的滚动窗口派生中立调度事实（`limit_reached` = 任一窗口触顶）。
pub(crate) fn scheduling_signals_from_snapshot(
    snapshot: &CodexAccountQuotaSnapshot,
) -> Option<AccountQuotaSignals> {
    let rolled = snapshot.clone().roll_expired_windows(SystemTime::now());
    quota_scheduling_signals(&rolled)
}

pub(crate) fn quota_scheduling_signals(
    snapshot: &CodexAccountQuotaSnapshot,
) -> Option<AccountQuotaSignals> {
    let now = SystemTime::now();
    let mut limit_reached = false;
    let mut remaining_rank: Option<u64> = None;
    let mut reset_at: Option<SystemTime> = None;
    for window in &snapshot.windows {
        limit_reached |= window.limit_reached();
        if let Some(used) = window
            .used_percent()
            .filter(|used| used.is_finite() && (0.0..=100.0).contains(used))
        {
            let rank = ((100.0 - used) * 100.0).round() as u64;
            remaining_rank = Some(remaining_rank.map_or(rank, |current| current.min(rank)));
        }
        if let Some(reset) = window
            .reset_at()
            .map(SystemTime::from)
            .filter(|reset| *reset > now)
        {
            reset_at = Some(reset_at.map_or(reset, |current| current.min(reset)));
        }
    }
    // 快照级 limit_reached 是调度排除的充分条件：即使没有任何窗口带
    // percent/reset（如顶层限流标记），也必须产出信号；不能返回 None 丢失排除。
    if limit_reached || remaining_rank.is_some() || reset_at.is_some() {
        Some(AccountQuotaSignals::new(
            reset_at,
            remaining_rank,
            limit_reached,
        ))
    } else {
        None
    }
}

pub(crate) fn parse_account_quota_snapshot(
    account_id: ProviderAccountId,
    credential_revision: CredentialRevision,
    observed_at: SystemTime,
    usage: &Value,
) -> Result<CodexAccountQuotaSnapshot, CodexCredentialQuotaError> {
    let fact = parse_codex_quota_usage(usage)?;
    let object = usage
        .as_object()
        .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
    let authoritative_core_primary_exhausted = authoritative_core_primary_exhausted(object)?;
    let mut windows = Vec::new();
    for limit in canonical_rate_limits(object)? {
        parse_rate_limit_windows(
            &limit.key,
            &limit.source,
            limit.limit_name.as_deref(),
            limit.rate_limit,
            &mut windows,
        )?;
    }
    windows.sort_by(|left, right| left.key.cmp(&right.key));
    Ok(CodexAccountQuotaSnapshot {
        account_id,
        credential_revision,
        observed_at,
        fact,
        authoritative_core_primary_exhausted,
        windows,
    })
}

/// 账号状态只能由顶层 canonical Codex 额度桶的明确拒绝来改变。
///
/// `fact.exhausted()` 和窗口 `limit_reached` 都会聚合或继承 secondary、附加桶等事实，
/// 因此不能单独用来将整个账号标为 `QuotaExhausted`。
fn authoritative_core_primary_exhausted(
    usage: &Map<String, Value>,
) -> Result<bool, CodexCredentialQuotaError> {
    let Some(rate_limit) = usage.get("rate_limit") else {
        return Ok(false);
    };
    if rate_limit.is_null() {
        return Ok(false);
    }
    let rate_limit = rate_limit
        .as_object()
        .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
    if optional_bool(rate_limit, "allowed")? != Some(false) {
        return Ok(false);
    }
    let Some(primary_window) = rate_limit.get("primary_window") else {
        return Ok(false);
    };
    if primary_window.is_null() {
        return Ok(false);
    }
    let primary_window = primary_window
        .as_object()
        .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
    let primary_window_reached = optional_bool(rate_limit, "limit_reached")?.unwrap_or(false)
        || optional_bool(primary_window, "limit_reached")?.unwrap_or(false)
        || primary_window
            .get("used_percent")
            .map(|value| {
                value
                    .as_f64()
                    .filter(|value| value.is_finite() && *value >= 0.0)
                    .map(|value| value >= 100.0)
                    .ok_or(CodexCredentialQuotaError::InvalidCredentialData)
            })
            .transpose()?
            .unwrap_or(false);
    Ok(primary_window_reached)
}

/// 一个规范化后的限流桶：`key` 用于稳定区分窗口，`source` 用于展示分组。
struct CanonicalRateLimit<'a> {
    key: String,
    source: String,
    limit_name: Option<String>,
    rate_limit: &'a Value,
}

fn canonical_rate_limits(
    object: &Map<String, Value>,
) -> Result<Vec<CanonicalRateLimit<'_>>, CodexCredentialQuotaError> {
    let mut limits = Vec::new();
    let has_primary_codex_rate_limit = object
        .get("rate_limit")
        .is_some_and(|value| !value.is_null());
    if let Some(rate_limit) = object.get("rate_limit") {
        // 官方 Codex 将顶层 `rate_limit` 固定识别为 `codex`，而不把
        // `active_limit` 投影成另一个展示桶。后者只是当前活动套餐的元数据。
        limits.push(CanonicalRateLimit {
            key: "core".to_owned(),
            source: "codex".to_owned(),
            limit_name: None,
            rate_limit,
        });
    }
    if let Some(rate_limit) = object
        .get("code_review_rate_limit")
        .filter(|value| !value.is_null())
    {
        limits.push(CanonicalRateLimit {
            key: "code-review".to_owned(),
            source: "code_review".to_owned(),
            limit_name: Some("code_review".to_owned()),
            rate_limit,
        });
    }
    let Some(additional) = object
        .get("additional_rate_limits")
        .filter(|value| !value.is_null())
    else {
        return Ok(limits);
    };
    for (index, value) in additional
        .as_array()
        .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?
        .iter()
        .enumerate()
    {
        let item = value
            .as_object()
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
        let (source, limit_name) = canonical_rate_limit_source(item, index);
        // 部分上游响应会在 `additional_rate_limits` 中重复默认的 `codex`
        // 桶。它没有独立的 limit id，不能在账号面板再次展示。
        if has_primary_codex_rate_limit && is_codex_limit_id(&source) {
            continue;
        }
        if let Some(rate_limit) = item.get("rate_limit") {
            // 非 Codex additional 桶保留为独立快照；额外用索引生成窗口键，
            // 避免同名 additional 产生重复前端 key。
            limits.push(CanonicalRateLimit {
                key: format!("additional-{index}-{source}"),
                source,
                limit_name,
                rate_limit,
            });
        }
    }
    Ok(limits)
}

fn is_codex_limit_id(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("codex")
}

fn canonical_rate_limit_source(
    item: &Map<String, Value>,
    index: usize,
) -> (String, Option<String>) {
    let source = item
        .get("metered_feature")
        .or_else(|| item.get("limit_name"))
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .unwrap_or_else(|| format!("additional-{index}"));
    let limit_name = item
        .get("limit_name")
        .or_else(|| item.get("metered_feature"))
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
        })
        .map(str::to_owned);
    (source, limit_name)
}

fn parse_rate_limit_windows(
    key_source: &str,
    source: &str,
    limit_name: Option<&str>,
    value: &Value,
    output: &mut Vec<CodexQuotaWindow>,
) -> Result<(), CodexCredentialQuotaError> {
    let object = value
        .as_object()
        .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
    // 顶层限流标记是快照级事实：任一窗口都要继承所属
    // rate_limit 的 `limit_reached=true` / `allowed=false`。
    let top_level_reached = optional_bool(object, "limit_reached")?.unwrap_or(false)
        || optional_bool(object, "allowed")?.is_some_and(|allowed| !allowed);
    for (name, role) in [
        ("primary_window", CodexQuotaWindowRole::Primary),
        ("secondary_window", CodexQuotaWindowRole::Secondary),
    ] {
        let Some(window) = object.get(name) else {
            continue;
        };
        if window.is_null() {
            continue;
        }
        let window = window
            .as_object()
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
        let window_seconds = optional_positive_u64(window, "limit_window_seconds")?
            .or(optional_positive_u64(window, "window_seconds")?)
            .or(optional_positive_u64(window, "window_minutes")?
                .and_then(|minutes| minutes.checked_mul(60)));
        let used_percent = window
            .get("used_percent")
            .map(|value| {
                value
                    .as_f64()
                    .filter(|value| value.is_finite() && *value >= 0.0)
                    .map(|value| value.clamp(0.0, 100.0))
                    .ok_or(CodexCredentialQuotaError::InvalidCredentialData)
            })
            .transpose()?;
        let reset_at = window
            .get("reset_at")
            .map(|value| {
                value
                    .as_i64()
                    .filter(|value| *value > 0)
                    .and_then(|value| DateTime::<Utc>::from_timestamp(value, 0))
                    .ok_or(CodexCredentialQuotaError::InvalidCredentialData)
            })
            .transpose()?;
        let limit_reached = top_level_reached
            || window
                .get("limit_reached")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || used_percent.is_some_and(|used| used >= 100.0);
        let kind = quota_window_kind(window_seconds);
        output.push(CodexQuotaWindow {
            key: format!("{}-{}", quota_key(key_source), quota_role_name(role, kind)),
            source: source.to_owned(),
            limit_name: limit_name.map(str::to_owned),
            kind,
            role,
            window_seconds,
            used_percent,
            reset_at,
            limit_reached,
        });
    }
    Ok(())
}

fn optional_positive_u64(
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<u64>, CodexCredentialQuotaError> {
    object
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .filter(|value| *value > 0)
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)
        })
        .transpose()
}

const fn quota_window_kind(seconds: Option<u64>) -> CodexQuotaWindowKind {
    match seconds {
        Some(value) if value >= 17_100 && value <= 18_900 => CodexQuotaWindowKind::ShortTerm,
        Some(value) if value >= 574_560 && value <= 635_040 => CodexQuotaWindowKind::Weekly,
        Some(value) if value >= 2_462_400 && value <= 2_721_600 => CodexQuotaWindowKind::Monthly,
        _ => CodexQuotaWindowKind::Other,
    }
}

fn quota_key(value: &str) -> String {
    let mut key = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while key.contains("--") {
        key = key.replace("--", "-");
    }
    let key = key.trim_matches('-');
    if key.is_empty() {
        "quota".to_owned()
    } else {
        key.to_owned()
    }
}

const fn quota_role_name(role: CodexQuotaWindowRole, kind: CodexQuotaWindowKind) -> &'static str {
    match kind {
        CodexQuotaWindowKind::ShortTerm => "five-hour",
        CodexQuotaWindowKind::Weekly => "weekly",
        CodexQuotaWindowKind::Monthly => "monthly",
        CodexQuotaWindowKind::Other => match role {
            CodexQuotaWindowRole::Primary => "primary",
            CodexQuotaWindowRole::Secondary => "secondary",
            CodexQuotaWindowRole::Monthly => "monthly",
        },
    }
}

#[derive(Default)]
struct QuotaAggregate {
    recognized: bool,
    exhausted: bool,
    remaining_percent: Option<u8>,
    resets_at: Option<DateTime<Utc>>,
}

impl QuotaAggregate {
    fn observe_rate_limit(&mut self, value: &Value) -> Result<(), CodexCredentialQuotaError> {
        let object = value
            .as_object()
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
        self.recognized = true;
        self.exhausted |= optional_bool(object, "limit_reached")?.unwrap_or(false);
        self.exhausted |= optional_bool(object, "allowed")?.is_some_and(|allowed| !allowed);
        for key in ["primary_window", "secondary_window"] {
            if let Some(window) = object.get(key) {
                self.observe_window(window)?;
            }
        }
        Ok(())
    }

    fn observe_window(&mut self, value: &Value) -> Result<(), CodexCredentialQuotaError> {
        if value.is_null() {
            return Ok(());
        }
        let object = value
            .as_object()
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
        if let Some(used) = object.get("used_percent") {
            let used = used
                .as_f64()
                .filter(|value| value.is_finite() && *value >= 0.0)
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
            // 子窗口触顶（used>=100）是快照级 exhaustion 事实。
            self.exhausted |= used >= 100.0;
            let remaining = (100.0 - used).clamp(0.0, 100.0).round() as u8;
            self.remaining_percent = Some(
                self.remaining_percent
                    .map_or(remaining, |current| current.min(remaining)),
            );
        }
        if let Some(reset) = object.get("reset_at") {
            let seconds = reset
                .as_i64()
                .filter(|seconds| *seconds > 0)
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
            let reset = DateTime::<Utc>::from_timestamp(seconds, 0)
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
            self.resets_at = Some(self.resets_at.map_or(reset, |current| current.min(reset)));
        }
        Ok(())
    }

    fn observe_exhaustion_object(
        &mut self,
        value: &Value,
        key: &str,
    ) -> Result<(), CodexCredentialQuotaError> {
        if value.is_null() {
            return Ok(());
        }
        let object = value
            .as_object()
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
        self.recognized = true;
        self.exhausted |= optional_bool(object, key)?.unwrap_or(false);
        Ok(())
    }
}

fn optional_bool(
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<bool>, CodexCredentialQuotaError> {
    object
        .get(key)
        .map(|value| {
            value
                .as_bool()
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)
        })
        .transpose()
}
