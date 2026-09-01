//! 快照/窗口解析、聚合、O(1) 滚动与调度信号。
//!
//! Provider raw quota JSON 的唯一解析者：产出 [`CodexAccountQuotaSnapshot`]
//! 供服务编排层消费；`limit_reached` 是快照级事实，不能只从子窗口反推。

use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use gateway_core::account::{
    AccountQuotaSignals, CredentialRevision, ProviderAccountId, QuotaEvidence, QuotaObservation,
    QuotaState,
};
use serde_json::{Map, Value};

use super::document::{
    DEFAULT_CODEX_LIMIT_ID, RATE_LIMITS_BY_LIMIT_ID, canonicalize_rate_limit_document,
};
use super::{CodexCredentialQuotaError, QUOTA_SCHEDULING_TTL};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexQuotaFact {
    remaining_percent: Option<u8>,
    resets_at: Option<DateTime<Utc>>,
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
    account_wide: bool,
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

    /// 该窗口是否覆盖账号全部 Codex 请求，可使用账号级本地用量聚合。
    #[must_use]
    pub const fn is_account_wide(&self) -> bool {
        self.account_wide
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
    quota: QuotaState,
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
    pub const fn quota(&self) -> QuotaState {
        self.quota
    }

    /// 将已持久化的规范化事实覆盖到 raw JSON 展示快照上。
    #[must_use]
    pub(crate) const fn with_quota_state(mut self, quota: QuotaState) -> Self {
        self.quota = quota;
        self
    }

    /// 只更新展示与调度使用的最后成功查询时间，保留既有额度事实。
    #[must_use]
    pub(crate) const fn with_observed_at(mut self, observed_at: SystemTime) -> Self {
        self.observed_at = observed_at;
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
}

pub fn parse_codex_quota_usage(usage: &Value) -> Result<CodexQuotaFact, CodexCredentialQuotaError> {
    let object = canonicalize_rate_limit_document(
        usage
            .as_object()
            .cloned()
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?,
    );
    parse_codex_quota_object(&object)
}

fn parse_codex_quota_object(
    object: &Map<String, Value>,
) -> Result<CodexQuotaFact, CodexCredentialQuotaError> {
    let mut aggregate = QuotaAggregate::default();
    for limit in canonical_rate_limits(object)? {
        aggregate.observe_rate_limit(limit.rate_limit)?;
    }
    // credits / spend_control 只证明文档结构已识别，不参与账号级访问结论。
    if let Some(spend_control) = object.get("spend_control") {
        aggregate.observe_metadata_object(spend_control, "reached")?;
    }
    if let Some(credits) = object.get("credits") {
        aggregate.observe_metadata_object(credits, "overage_limit_reached")?;
    }
    if !aggregate.recognized {
        return Err(CodexCredentialQuotaError::InvalidCredentialData);
    }
    Ok(CodexQuotaFact {
        remaining_percent: aggregate.remaining_percent,
        resets_at: aggregate.resets_at,
    })
}

fn canonical_quota_object(usage: &Value) -> Result<Map<String, Value>, CodexCredentialQuotaError> {
    Ok(canonicalize_rate_limit_document(
        usage
            .as_object()
            .cloned()
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?,
    ))
}

pub(crate) fn quota_snapshot_from_observation(
    observation: &QuotaObservation,
) -> Option<CodexAccountQuotaSnapshot> {
    let observed_at = observation.observed_at;
    let quota = &observation.quota;
    let mut snapshot = parse_account_quota_snapshot(
        observation.account_id.clone(),
        observation.expected_revision,
        observed_at,
        &Value::Object(quota.expose_to_provider().clone()),
    )
    .ok()?;
    // 归一化列是持久化权威事实；raw JSON 只负责 Provider 展示结构。
    snapshot.quota = observation.state;
    Some(snapshot)
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

/// 从快照窗口派生容量排序事实；额度访问资格由 `QuotaState` 统一判断。
pub(crate) fn scheduling_signals_from_snapshot(
    snapshot: &CodexAccountQuotaSnapshot,
) -> Option<AccountQuotaSignals> {
    quota_scheduling_signals(snapshot)
}

pub(crate) fn quota_scheduling_signals(
    snapshot: &CodexAccountQuotaSnapshot,
) -> Option<AccountQuotaSignals> {
    let now = SystemTime::now();
    let mut remaining_rank: Option<u64> = None;
    let mut reset_at: Option<SystemTime> = None;
    for window in &snapshot.windows {
        let window_reset_at = window.reset_at().map(SystemTime::from);
        let observation_expired = window_reset_at.is_some_and(|reset_at| reset_at <= now);
        if !observation_expired
            && let Some(used) = window
                .used_percent()
                .filter(|used| used.is_finite() && (0.0..=100.0).contains(used))
        {
            let rank = ((100.0 - used) * 100.0).round() as u64;
            remaining_rank = Some(remaining_rank.map_or(rank, |current| current.min(rank)));
        }
        if let Some(reset) = window_reset_at.filter(|reset| *reset > now) {
            reset_at = Some(reset_at.map_or(reset, |current| current.min(reset)));
        }
    }
    if remaining_rank.is_some() || reset_at.is_some() {
        Some(AccountQuotaSignals::new(reset_at, remaining_rank))
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
    let object = canonical_quota_object(usage)?;
    let fact = parse_codex_quota_object(&object)?;
    let quota = authoritative_quota_state(&object, observed_at)?;
    let mut windows = Vec::new();
    for limit in canonical_rate_limits(&object)? {
        parse_rate_limit_windows(
            &limit.key,
            &limit.source,
            limit.account_wide,
            limit.limit_name.as_deref(),
            limit.rate_limit,
            &mut windows,
        )?;
    }
    windows.sort_by(|left, right| {
        quota_source_order(left.source())
            .cmp(&quota_source_order(right.source()))
            .then_with(|| left.key.cmp(&right.key))
    });
    Ok(CodexAccountQuotaSnapshot {
        account_id,
        credential_revision,
        observed_at,
        fact,
        quota,
        windows,
    })
}

fn quota_source_order(source: &str) -> u8 {
    match source {
        "codex" => 0,
        "code_review" => 1,
        _ => 2,
    }
}

/// 默认 `codex` 桶是唯一可改变账号额度访问结论的 quota 响应字段。
fn authoritative_quota_state(
    usage: &Map<String, Value>,
    observed_at: SystemTime,
) -> Result<QuotaState, CodexCredentialQuotaError> {
    let Some(rate_limit) = usage
        .get(RATE_LIMITS_BY_LIMIT_ID)
        .and_then(Value::as_object)
        .and_then(|limits| limits.get(DEFAULT_CODEX_LIMIT_ID))
    else {
        return Ok(QuotaState::observed_unknown(observed_at));
    };
    if rate_limit.is_null() {
        return Ok(QuotaState::observed_unknown(observed_at));
    }
    let rate_limit = rate_limit
        .as_object()
        .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
    let allowed = optional_bool(rate_limit, "allowed")?;
    let top_level_reached = optional_bool(rate_limit, "limit_reached")?.unwrap_or(false);
    let primary_window = rate_limit
        .get("primary_window")
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_object()
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)
        })
        .transpose()?;
    let primary_reached = primary_window
        .map(|window| optional_bool(window, "limit_reached"))
        .transpose()?
        .flatten()
        .unwrap_or(false);
    let reset_at = primary_window
        .and_then(|window| window.get("reset_at"))
        .map(|value| {
            value
                .as_i64()
                .filter(|value| *value > 0)
                .and_then(|value| DateTime::<Utc>::from_timestamp(value, 0))
                .map(SystemTime::from)
                .ok_or(CodexCredentialQuotaError::InvalidCredentialData)
        })
        .transpose()?;

    if allowed == Some(true) {
        if top_level_reached || primary_reached {
            tracing::warn!(
                top_level_reached,
                primary_reached,
                "OpenAI quota returned contradictory access facts; explicit allowed=true wins"
            );
        }
        return Ok(QuotaState::allowed(observed_at));
    }
    if allowed == Some(false) {
        return Ok(QuotaState::exhausted(
            QuotaEvidence::ProviderDenied,
            observed_at,
            reset_at,
        ));
    }
    if top_level_reached || primary_reached {
        return Ok(QuotaState::exhausted(
            QuotaEvidence::AccountLimitReached,
            observed_at,
            reset_at,
        ));
    }
    Ok(QuotaState::observed_unknown(observed_at))
}

/// 一个规范化后的限流桶：`key` 用于稳定区分窗口，`source` 用于展示分组。
struct CanonicalRateLimit<'a> {
    key: String,
    source: String,
    account_wide: bool,
    limit_name: Option<String>,
    rate_limit: &'a Value,
}

fn canonical_rate_limits(
    object: &Map<String, Value>,
) -> Result<Vec<CanonicalRateLimit<'_>>, CodexCredentialQuotaError> {
    let mut limits = Vec::new();
    let Some(by_limit_id) = object.get(RATE_LIMITS_BY_LIMIT_ID) else {
        return Ok(limits);
    };
    for (limit_id, rate_limit) in by_limit_id
        .as_object()
        .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?
    {
        let item = rate_limit
            .as_object()
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
        let source = item
            .get("limit_id")
            .and_then(Value::as_str)
            .unwrap_or(limit_id)
            .to_owned();
        let limit_name = item
            .get("limit_name")
            .and_then(Value::as_str)
            .map(str::to_owned);
        limits.push(CanonicalRateLimit {
            key: source.clone(),
            account_wide: is_codex_limit_id(&source),
            source,
            limit_name,
            rate_limit,
        });
    }
    Ok(limits)
}

fn is_codex_limit_id(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("codex")
}

fn parse_rate_limit_windows(
    key_source: &str,
    source: &str,
    account_wide: bool,
    limit_name: Option<&str>,
    value: &Value,
    output: &mut Vec<CodexQuotaWindow>,
) -> Result<(), CodexCredentialQuotaError> {
    let object = value
        .as_object()
        .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
    let allowed = optional_bool(object, "allowed")?;
    let top_level_reached = allowed != Some(true)
        && (optional_bool(object, "limit_reached")?.unwrap_or(false) || allowed == Some(false));
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
        let limit_reached = allowed != Some(true)
            && (top_level_reached
                || window
                    .get("limit_reached")
                    .and_then(Value::as_bool)
                    .unwrap_or(false));
        let kind = quota_window_kind(window_seconds);
        output.push(CodexQuotaWindow {
            key: format!("{}-{}", quota_key(key_source), quota_role_name(role, kind)),
            source: source.to_owned(),
            account_wide,
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
    remaining_percent: Option<u8>,
    resets_at: Option<DateTime<Utc>>,
}

impl QuotaAggregate {
    fn observe_rate_limit(&mut self, value: &Value) -> Result<(), CodexCredentialQuotaError> {
        let object = value
            .as_object()
            .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?;
        self.recognized = true;
        let _ = optional_bool(object, "limit_reached")?;
        let _ = optional_bool(object, "allowed")?;
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

    fn observe_metadata_object(
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
        let _ = optional_bool(object, key)?;
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
