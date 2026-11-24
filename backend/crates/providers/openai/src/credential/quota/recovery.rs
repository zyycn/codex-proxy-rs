//! 将耗尽窗口的恢复基准与最新用量分开持久化，允许各窗口分批恢复

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use gateway_core::account::{QuotaAccessState, QuotaEvidence, QuotaState};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{CodexAccountQuotaSnapshot, CodexCredentialQuotaError, CodexQuotaWindow};

pub(super) const RECOVERY_FIELD: &str = "_quota_recovery";
const RESET_RECOVERY_MAX_USED_PERCENT: f64 = 10.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct QuotaRecovery {
    // 绑定本次耗尽事实，避免真实推理成功后再次耗尽时复用上次的恢复进度
    exhausted_at_micros: i64,
    pending: BTreeMap<String, Option<DateTime<Utc>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unmatched: Option<UnmatchedQuotaReset>,
    // 上次刷新已观察到未触顶的窗口，下一次仍有证据时解除
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    candidates: BTreeMap<String, ()>,
}

// 拒绝给出了复查时间，但快照无法将它归属到具体窗口
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct UnmatchedQuotaReset {
    reset_at: DateTime<Utc>,
    window_keys: BTreeSet<String>,
}

impl UnmatchedQuotaReset {
    fn recovered(&self, refreshed: &CodexAccountQuotaSnapshot) -> bool {
        let windows = refreshed.windows().iter().filter(|w| w.is_account_wide());
        SystemTime::from(self.reset_at) <= refreshed.observed_at()
            && refreshed.quota().access() == QuotaAccessState::Allowed
            && windows.clone().next().is_some()
            && windows
                .clone()
                .all(|w| w.used_percent().is_some() && !w.limit_reached())
            && self
                .window_keys
                .iter()
                .all(|key| windows.clone().any(|w| w.key() == key))
    }
}

impl QuotaRecovery {
    fn new(state: QuotaState, baseline: &CodexAccountQuotaSnapshot) -> Self {
        let windows = || baseline.windows().iter().filter(|w| w.is_account_wide());
        let mut pending = BTreeMap::new();
        for window in windows() {
            if window.limit_reached() {
                pending.insert(window.key().to_owned(), window.reset_at());
            }
        }
        // 旧版从 primary 派生的账号 reset 不能覆盖明确的周窗口耗尽事实
        // 真实 usage_limit_reached 错误则可以补充尚未反映到百分比的耗尽窗口
        let error_deadline = state
            .reset_at()
            .filter(|at| {
                state.evidence() == Some(QuotaEvidence::UsageLimitReached)
                    && state.observed_at().is_some_and(|denied| *at > denied)
            })
            .map(DateTime::<Utc>::from);
        let hinted = baseline.denied_window(state);
        // 错误时间与快照周期不一致时，不能用错误时间替代窗口基线并误判已滚动
        let conflicting_reset = hinted
            .and_then(CodexQuotaWindow::reset_at)
            .zip(error_deadline)
            .is_some_and(|(window, error)| (window.timestamp() - error.timestamp()).abs() > 1);
        if let Some(window) = hinted {
            if !conflicting_reset {
                pending.insert(
                    window.key().to_owned(),
                    window.reset_at().or(error_deadline),
                );
            }
        } else if pending.is_empty() || state.evidence() == Some(QuotaEvidence::UsageLimitReached) {
            for window in windows() {
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
        let unmatched = error_deadline
            .filter(|_| pending.is_empty() || conflicting_reset)
            .map(|reset_at| UnmatchedQuotaReset {
                reset_at,
                window_keys: windows().map(|w| w.key().to_owned()).collect(),
            });
        // 缺少可信的错误边界时保留原有的逐窗口复核
        if pending.is_empty() && unmatched.is_none() {
            pending.extend(windows().map(|w| (w.key().to_owned(), w.reset_at())));
        }
        Self {
            exhausted_at_micros: exhaustion_time(state),
            pending,
            unmatched,
            candidates: BTreeMap::new(),
        }
    }

    fn observe(&mut self, refreshed: &CodexAccountQuotaSnapshot, has_window_hint: bool) -> bool {
        let windows = || refreshed.windows().iter().filter(|w| w.is_account_wide());
        let access = refreshed.quota().access();
        // 拒绝仍有效时，即便被点名的窗口已滚动，也要等待账号放行
        if has_window_hint && access != QuotaAccessState::Allowed {
            self.candidates.clear();
            for window in windows().filter(|w| w.limit_reached()) {
                self.pending
                    .entry(window.key().to_owned())
                    .or_insert(window.reset_at());
            }
            return false;
        }
        let had_baseline = self.unmatched.is_some() || !self.pending.is_empty();
        self.unmatched.take_if(|reset| reset.recovered(refreshed));
        // 已确认窗口全部到期后，明确放行可以覆盖滞后的百分比
        if !self.pending.is_empty()
            && access == QuotaAccessState::Allowed
            && self.pending.iter().all(|(key, reset)| {
                let reported = !has_window_hint
                    || windows().any(|w| w.key() == key && w.used_percent().is_some());
                reported
                    && reset.is_some_and(|reset| {
                        reset.timestamp_micros() > self.exhausted_at_micros
                            && SystemTime::from(reset) <= refreshed.observed_at()
                    })
            })
        {
            self.pending.clear();
            self.candidates.clear();
            return self.unmatched.is_none();
        }
        let mut candidates = BTreeMap::new();
        for window in windows() {
            let key = window.key();
            if let Some(previous_reset) = self.pending.get(key).copied() {
                let Some(previous_reset) = previous_reset else {
                    // 首次观测到该窗口只锚定 reset 基线，不构成解除证据
                    if !has_window_hint || window.used_percent().is_some() {
                        self.pending.insert(key.to_owned(), window.reset_at());
                    }
                    continue;
                };
                let Some(used) = window.used_percent() else {
                    continue;
                };
                if window.limit_reached() {
                    continue;
                }
                let advanced = window.reset_at().is_some_and(|at| at > previous_reset);
                if advanced && used < RESET_RECOVERY_MAX_USED_PERCENT {
                    self.pending.remove(key);
                    continue;
                }
                // 明确放行时，reset 的位移不影响连续两次未触顶的证据
                // 上游未给出放行状态时，只沿用同一窗口内用量回落的确认规则
                if access == QuotaAccessState::Allowed
                    || (access == QuotaAccessState::Unknown && !advanced)
                {
                    if self.candidates.contains_key(key) {
                        self.pending.remove(key);
                    } else {
                        candidates.insert(key.to_owned(), ());
                    }
                }
            } else if window.limit_reached() {
                self.pending.insert(key.to_owned(), window.reset_at());
            }
        }
        self.candidates = candidates;
        had_baseline && self.pending.is_empty() && self.unmatched.is_none()
    }
}

fn exhaustion_time(state: QuotaState) -> i64 {
    DateTime::<Utc>::from(state.observed_at().unwrap_or(SystemTime::UNIX_EPOCH)).timestamp_micros()
}

/// 统一手动刷新和 worker 的恢复规则
/// 已记录的耗尽窗口全部到期后，允许新的明确放行观察解除，兼容用量百分比滞后
pub(super) fn reconcile_refresh(
    current: QuotaState,
    auto_use_credits: bool,
    refreshed: &mut CodexAccountQuotaSnapshot,
    previous: Option<&CodexAccountQuotaSnapshot>,
    document: &mut Map<String, Value>,
) -> Result<QuotaState, CodexCredentialQuotaError> {
    document.remove(RECOVERY_FIELD);
    // 点数已不再可用时，旧点数依据不能随下一次开关切换复活
    let state = if current.evidence() == Some(QuotaEvidence::CreditsAvailable) {
        QuotaState::exhausted(
            QuotaEvidence::AccountLimitReached,
            current.observed_at().unwrap_or(refreshed.observed_at()),
            current.reset_at(),
        )
        .merge_observation(refreshed.quota())
    } else if current.is_exhausted() {
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
    if recovery.pending.is_empty() && recovery.unmatched.is_none() {
        recovery = QuotaRecovery::new(state, refreshed);
    }
    if current.is_exhausted() && recovery.observe(refreshed, state.limit_window_minutes().is_some())
    {
        refreshed.recovery = None;
        return Ok(QuotaState::allowed(refreshed.observed_at()));
    }
    let reset_at = recovery
        .unmatched
        .as_ref()
        .map(|reset| reset.reset_at)
        .or_else(|| recovery.pending.values().filter_map(|reset| *reset).min())
        .map(SystemTime::from)
        .or(state.reset_at());
    let state = refreshed.access_with_credits(
        QuotaState::exhausted(
            state
                .evidence()
                .unwrap_or(QuotaEvidence::AccountLimitReached),
            state.observed_at().unwrap_or(refreshed.observed_at()),
            reset_at,
        )
        .with_limit_window_minutes(state.limit_window_minutes()),
        auto_use_credits,
    );
    if !state.is_exhausted() {
        refreshed.recovery = None;
        return Ok(state);
    }
    document.insert(
        RECOVERY_FIELD.to_owned(),
        serde_json::to_value(&recovery)
            .map_err(|_| CodexCredentialQuotaError::InvalidCredentialData)?,
    );
    refreshed.recovery = Some(recovery);
    Ok(state)
}
