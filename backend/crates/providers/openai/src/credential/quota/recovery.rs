//! 将耗尽窗口的恢复基准与最新用量分开持久化，允许各窗口分批恢复

use std::collections::BTreeMap;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use gateway_core::account::{
    QuotaEvidence, QuotaState, QuotaWindowRecovery, quota_window_recovery,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{CodexAccountQuotaSnapshot, CodexCredentialQuotaError};

pub(super) const RECOVERY_FIELD: &str = "_quota_recovery";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct QuotaRecovery {
    // 绑定本次耗尽事实，避免真实推理成功后再次耗尽时复用上次的恢复进度
    exhausted_at_micros: i64,
    pending: BTreeMap<String, Option<DateTime<Utc>>>,
    // 已连续观测到"未触顶"的待解除窗口；第二次确认后解除
    // 旧文档没有该
    // 字段时反序列化为空集，从当前耗尽事实重新积累证据
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    candidates: BTreeMap<String, ()>,
}

impl QuotaRecovery {
    fn new(state: QuotaState, baseline: &CodexAccountQuotaSnapshot) -> Self {
        let mut pending = BTreeMap::new();
        for window in baseline.windows().iter().filter(|w| w.is_account_wide()) {
            if window.limit_reached() {
                pending.insert(window.key().to_owned(), window.reset_at());
            }
        }
        // 旧版从 primary 派生的账号 reset 不能覆盖明确的周窗口耗尽事实
        // 真实 usage_limit_reached 错误则可以补充尚未反映到百分比的耗尽窗口
        if pending.is_empty() || state.evidence() == Some(QuotaEvidence::UsageLimitReached) {
            for window in baseline.windows().iter().filter(|w| w.is_account_wide()) {
                // 错误与 usage 的秒级 reset 可能相差一秒；只用它识别窗口，
                // 后续推进比较始终使用该窗口自己的 reset
                let matches_error_reset = window.reset_at().zip(state.reset_at()).is_some_and(
                    |(window_reset, error_reset)| {
                        (window_reset.timestamp() - DateTime::<Utc>::from(error_reset).timestamp())
                            .abs()
                            <= 1
                    },
                );
                if matches_error_reset {
                    pending.insert(window.key().to_owned(), window.reset_at());
                }
            }
        }
        // 上游未指出耗尽窗口时保留所有候选，不能任意把错误归给 primary
        if pending.is_empty() {
            pending.extend(
                baseline
                    .windows()
                    .iter()
                    .filter(|w| w.is_account_wide())
                    .map(|w| (w.key().to_owned(), w.reset_at())),
            );
        }
        Self {
            exhausted_at_micros: exhaustion_time(state),
            pending,
            candidates: BTreeMap::new(),
        }
    }

    fn observe(&mut self, refreshed: &CodexAccountQuotaSnapshot) -> bool {
        let had_baseline = !self.pending.is_empty();
        let mut candidates = BTreeMap::new();
        for window in refreshed.windows().iter().filter(|w| w.is_account_wide()) {
            let key = window.key();
            if let Some(previous_reset) = self.pending.get(key).copied() {
                match quota_window_recovery(
                    previous_reset.map(SystemTime::from),
                    window.reset_at().map(SystemTime::from),
                    window.used_percent(),
                    window.limit_reached(),
                    self.candidates.contains_key(key),
                ) {
                    QuotaWindowRecovery::Recovered => {
                        self.pending.remove(key);
                    }
                    QuotaWindowRecovery::Candidate => {
                        candidates.insert(key.to_owned(), ());
                    }
                    QuotaWindowRecovery::Pending => {
                        if previous_reset.is_none() {
                            self.pending.insert(key.to_owned(), window.reset_at());
                        }
                    }
                }
            } else if window.limit_reached() {
                self.pending.insert(key.to_owned(), window.reset_at());
            }
        }
        self.candidates = candidates;
        had_baseline && self.pending.is_empty()
    }
}

fn exhaustion_time(state: QuotaState) -> i64 {
    DateTime::<Utc>::from(state.observed_at().unwrap_or(SystemTime::UNIX_EPOCH)).timestamp_micros()
}

/// 统一手动刷新和 worker 的恢复规则
/// 已耗尽窗口的解除有两条证据路径：
/// reset 前进且用量低于低用量阈值（窗口已滚动），或同一窗口内连续两次观测
/// 都未触顶（滑动窗口的用量回落，reset 不滚动）
/// 后者防止账号被钉在旧
/// reset 日期上；连续两次的要求排除耗尽后立刻拉到的旧快照
pub(super) fn reconcile_refresh(
    current: QuotaState,
    refreshed: &mut CodexAccountQuotaSnapshot,
    previous: Option<&CodexAccountQuotaSnapshot>,
    document: &mut Map<String, Value>,
) -> Result<QuotaState, CodexCredentialQuotaError> {
    document.remove(RECOVERY_FIELD);
    let state = if current.is_exhausted() {
        current
    } else {
        current.merge_observation(refreshed.quota())
    };
    if !state.is_exhausted() {
        return Ok(state);
    }
    let mut recovery = previous
        .and_then(|previous| previous.recovery.as_ref())
        .filter(|recovery| recovery.exhausted_at_micros == exhaustion_time(state))
        .cloned()
        .unwrap_or_else(|| QuotaRecovery::new(state, previous.unwrap_or(refreshed)));
    if recovery.pending.is_empty() {
        recovery = QuotaRecovery::new(state, refreshed);
    }
    if current.is_exhausted() && recovery.observe(refreshed) {
        return Ok(QuotaState::allowed(refreshed.observed_at()));
    }
    let reset_at = recovery
        .pending
        .values()
        .filter_map(|reset| *reset)
        .min()
        .map(SystemTime::from)
        .or(state.reset_at());
    document.insert(
        RECOVERY_FIELD.to_owned(),
        serde_json::to_value(&recovery)
            .map_err(|_| CodexCredentialQuotaError::InvalidCredentialData)?,
    );
    refreshed.recovery = Some(recovery);
    Ok(QuotaState::exhausted(
        state
            .evidence()
            .unwrap_or(QuotaEvidence::AccountLimitReached),
        state.observed_at().unwrap_or(refreshed.observed_at()),
        reset_at,
    ))
}
