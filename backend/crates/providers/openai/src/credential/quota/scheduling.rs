//! 请求级额度投影、首查与周期刷新排程

use super::*;
use tokio::time::Instant as TokioInstant;

impl CodexCredentialQuotaService {
    /// 批量预热请求级额度投影；持久层或 Provider JSON 异常只退化为未知额度
    pub async fn prepare_scheduling(&self, accounts: &[ProviderAccount]) {
        if self.scheduling.hydration_targets(accounts).is_empty() {
            return;
        }
        let _hydration = self.scheduling.hydration.lock().await;
        let pending = self.scheduling.hydration_targets(accounts);
        if pending.is_empty() {
            return;
        }
        let account_ids = pending
            .iter()
            .map(|target| target.account.id().clone())
            .collect::<Vec<_>>();
        let Ok(observations) = self.store.get_quotas(&account_ids).await else {
            for target in &pending {
                self.scheduling
                    .mark_unknown_if_unchanged(target, QUOTA_HYDRATION_FAILURE_TTL);
            }
            return;
        };
        let observations = observations
            .into_iter()
            .map(|observation| (observation.account_id.clone(), observation))
            .collect::<BTreeMap<_, _>>();
        for target in pending {
            let snapshot = observations
                .get(target.account.id())
                .filter(|observation| observation.expected_revision == target.account.revision())
                .and_then(quota_snapshot_from_observation);
            if !snapshot
                .is_some_and(|snapshot| self.scheduling.observe_if_unchanged(&target, &snapshot))
            {
                self.scheduling
                    .mark_unknown_if_unchanged(&target, QUOTA_SCHEDULING_TTL);
            }
        }
    }

    #[must_use]
    pub fn scheduling_signals(&self, account: &ProviderAccount) -> Option<AccountQuotaSignals> {
        self.scheduling.signals(account)
    }

    pub(crate) fn invalidate_scheduling(&self, account_ids: &[ProviderAccountId]) {
        self.scheduling.invalidate(account_ids);
    }
}

#[derive(Clone, Default)]
pub(super) struct CodexQuotaSchedulingProjection {
    state: Arc<RwLock<CodexQuotaProjectionState>>,
    hydration: Arc<Mutex<()>>,
}

#[derive(Default)]
struct CodexQuotaProjectionState {
    next_version: u64,
    entries: BTreeMap<ProviderAccountId, CodexQuotaSchedulingEntry>,
    last_periodic_refresh_at: BTreeMap<ProviderAccountId, CodexQuotaRefreshAttempt>,
    initial_refresh_not_before: BTreeMap<ProviderAccountId, Instant>,
}

struct CodexQuotaRefreshAttempt {
    monotonic_at: TokioInstant,
    wall_at: SystemTime,
}

#[derive(Debug, Clone, Copy)]
struct CodexQuotaSchedulingEntry {
    version: u64,
    revision: CredentialRevision,
    expires_at: Instant,
    signals: Option<AccountQuotaSignals>,
}

#[derive(Clone)]
struct CodexQuotaHydrationTarget {
    account: ProviderAccount,
    expected_version: Option<u64>,
}

impl CodexQuotaSchedulingProjection {
    fn invalidate(&self, account_ids: &[ProviderAccountId]) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for account_id in account_ids {
            state.entries.remove(account_id);
            state.last_periodic_refresh_at.remove(account_id);
            state.initial_refresh_not_before.remove(account_id);
        }
    }

    fn hydration_targets(&self, accounts: &[ProviderAccount]) -> Vec<CodexQuotaHydrationTarget> {
        let state = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        accounts
            .iter()
            .filter_map(|account| {
                let current = state.entries.get(account.id());
                current
                    .is_none_or(|entry| {
                        entry.revision != account.revision() || now >= entry.expires_at
                    })
                    .then(|| CodexQuotaHydrationTarget {
                        account: account.clone(),
                        expected_version: current.map(|entry| entry.version),
                    })
            })
            .collect()
    }

    pub(super) fn observe(&self, snapshot: &CodexAccountQuotaSnapshot) -> bool {
        let Some(remaining_ttl) = quota_projection_ttl(snapshot) else {
            return false;
        };
        self.replace(
            snapshot.account_id().clone(),
            snapshot.credential_revision(),
            remaining_ttl,
            scheduling_signals_from_snapshot(snapshot),
        );
        true
    }

    fn mark_unknown_if_unchanged(&self, target: &CodexQuotaHydrationTarget, ttl: Duration) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .entries
            .get(target.account.id())
            .map(|entry| entry.version)
            != target.expected_version
        {
            return;
        }
        insert_projection_entry(
            &mut state,
            target.account.id().clone(),
            target.account.revision(),
            ttl,
            None,
        );
    }

    fn observe_if_unchanged(
        &self,
        target: &CodexQuotaHydrationTarget,
        snapshot: &CodexAccountQuotaSnapshot,
    ) -> bool {
        let Some(remaining_ttl) = quota_projection_ttl(snapshot) else {
            return false;
        };
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .entries
            .get(target.account.id())
            .map(|entry| entry.version)
            != target.expected_version
        {
            return true;
        }
        insert_projection_entry(
            &mut state,
            snapshot.account_id().clone(),
            snapshot.credential_revision(),
            remaining_ttl,
            scheduling_signals_from_snapshot(snapshot),
        );
        true
    }

    fn replace(
        &self,
        account_id: ProviderAccountId,
        revision: CredentialRevision,
        ttl: Duration,
        signals: Option<AccountQuotaSignals>,
    ) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        insert_projection_entry(&mut state, account_id, revision, ttl, signals);
    }

    fn signals(&self, account: &ProviderAccount) -> Option<AccountQuotaSignals> {
        let state = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .entries
            .get(account.id())
            .filter(|entry| {
                entry.revision == account.revision() && Instant::now() < entry.expires_at
            })
            .and_then(|entry| entry.signals)
    }

    pub(super) fn reserve_periodic_refreshes(
        &self,
        accounts: Vec<ProviderAccount>,
        observed_snapshots: &BTreeMap<ProviderAccountId, CodexAccountQuotaSnapshot>,
        now: SystemTime,
    ) -> Vec<ProviderAccount> {
        let candidates = accounts
            .into_iter()
            .filter_map(|account| {
                let snapshot = observed_snapshots.get(account.id());
                quota_refresh_candidate(account, snapshot, now)
            })
            .collect::<Vec<_>>();
        let candidate_ids = candidates
            .iter()
            .map(|(account, _)| account.id().clone())
            .collect::<BTreeSet<_>>();
        let refreshed_at = TokioInstant::now();
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .last_periodic_refresh_at
            .retain(|account_id, _| candidate_ids.contains(account_id));

        // 已耗尽账号到达 reset 后短周期复核，其余时间保留 30 分钟兜底
        // 正常账号仅在非零用量窗口经过宽限期后参与，未更新时复用周期节流
        let mut reserved = Vec::new();
        for (account, target_reset) in candidates {
            if !periodic_quota_refresh_due(&state, &account, target_reset, now, refreshed_at) {
                continue;
            }
            state.last_periodic_refresh_at.insert(
                account.id().clone(),
                CodexQuotaRefreshAttempt {
                    monotonic_at: refreshed_at,
                    wall_at: now,
                },
            );
            reserved.push(account);
        }
        reserved
    }

    /// 为尚无 quota 快照的账号分配随机首查时点，并返回本轮已到期的候选。
    ///
    /// `quota_observed_at` 为空代表首次异步观察尚未成功；不另建同步状态表。
    /// 首次进入候选的账号抽一次随机启动延迟记为到期时刻，避免新导入账号池
    /// 在 worker 唤醒边界形成批量 `/usage` 突发；到期时刻跨轮保留，零延迟
    /// （含随机源退化）时保持首轮即查的旧行为。单轮数量仍受
    /// `INITIAL_QUOTA_SYNC_BATCH` 上限约束。
    pub(super) fn reserve_initial_refreshes(
        &self,
        accounts: &[ProviderAccount],
        observed_ids: &BTreeSet<ProviderAccountId>,
        now: SystemTime,
        draw_start_delay: &dyn Fn() -> Duration,
    ) -> Vec<ProviderAccount> {
        let candidates = accounts
            .iter()
            .filter(|account| {
                !observed_ids.contains(account.id()) && eligible_initial_quota_sync(account, now)
            })
            .collect::<Vec<_>>();
        let candidate_ids = candidates
            .iter()
            .map(|account| account.id().clone())
            .collect::<BTreeSet<_>>();
        let monotonic_now = Instant::now();
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // 已产生快照或离开候选集的账号不再保留首查状态。
        state
            .initial_refresh_not_before
            .retain(|account_id, _| candidate_ids.contains(account_id));
        candidates
            .into_iter()
            .filter(|account| {
                let due_at = state
                    .initial_refresh_not_before
                    .entry(account.id().clone())
                    .or_insert_with(|| monotonic_now + draw_start_delay());
                *due_at <= monotonic_now
            })
            .take(INITIAL_QUOTA_SYNC_BATCH)
            .cloned()
            .collect()
    }

    /// 首查尝试后仍无快照的账号重抽随机延迟，避免失败账号回到固定 30s 重试节奏。
    ///
    /// 重排同时随机化每轮的候选集合，避免持续失败时按账号顺序只重试同一批。
    pub(super) fn defer_failed_initial_refreshes(
        &self,
        account_ids: &BTreeSet<ProviderAccountId>,
        draw_start_delay: &dyn Fn() -> Duration,
    ) {
        if account_ids.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for account_id in account_ids {
            // 账号是否仍在首查路径由下一轮 reserve 按候选集剪枝，这里无需区分。
            state
                .initial_refresh_not_before
                .insert(account_id.clone(), now + draw_start_delay());
        }
    }
}

fn quota_refresh_candidate(
    account: ProviderAccount,
    snapshot: Option<&CodexAccountQuotaSnapshot>,
    now: SystemTime,
) -> Option<(ProviderAccount, Option<SystemTime>)> {
    if !eligible_periodic_quota_refresh(&account, now) {
        return None;
    }
    if account.quota().is_exhausted() {
        let reset_at = account.quota().reset_at();
        return Some((account, reset_at));
    }
    // 正常账号首次进入周期候选也要等待宽限期，不能由“无刷新历史”绕过
    let snapshot = snapshot?;
    let expired_window_reset = snapshot
        .windows()
        .iter()
        .filter(|window| {
            window.used_percent().is_some_and(|used| used > 0.0) || window.limit_reached()
        })
        .filter_map(CodexQuotaWindow::reset_at)
        .map(SystemTime::from)
        .filter(|reset| {
            reset
                .checked_add(QUOTA_RESET_GRACE)
                .is_some_and(|due_at| due_at <= now)
        })
        .min();
    expired_window_reset.map(|reset_at| (account, Some(reset_at)))
}

fn periodic_quota_refresh_due(
    state: &CodexQuotaProjectionState,
    account: &ProviderAccount,
    reset_at: Option<SystemTime>,
    now: SystemTime,
    monotonic_now: TokioInstant,
) -> bool {
    let mut interval = PERIODIC_QUOTA_REFRESH_RETRY_INTERVAL;
    let due_at = if account.quota().is_exhausted() {
        if let Some(since_reset) = reset_at.and_then(|reset| now.duration_since(reset).ok()) {
            // 上游边界附近可能仍返回旧额度，首次复核也保留传播余量
            if since_reset < EXHAUSTED_QUOTA_RESET_GRACE {
                return false;
            }
            interval = if since_reset < QUOTA_RESET_FAST_REFRESH_WINDOW {
                QUOTA_RESET_FAST_REFRESH_INTERVAL
            } else {
                QUOTA_RESET_REFRESH_RETRY_INTERVAL
            };
        }
        reset_at.and_then(|reset| reset.checked_add(EXHAUSTED_QUOTA_RESET_GRACE))
    } else {
        reset_at.and_then(|reset| reset.checked_add(QUOTA_RESET_GRACE))
    };
    let Some(last) = state.last_periodic_refresh_at.get(account.id()) else {
        return true;
    };
    monotonic_now.saturating_duration_since(last.monotonic_at) >= interval
        // 跨越 reset 宽限期的首轮复核不受旧间隔阻挡，之后按对应间隔节流
        || due_at.is_some_and(|due_at| last.wall_at < due_at && due_at <= now)
}

fn insert_projection_entry(
    state: &mut CodexQuotaProjectionState,
    account_id: ProviderAccountId,
    revision: CredentialRevision,
    ttl: Duration,
    signals: Option<AccountQuotaSignals>,
) {
    state.next_version = state.next_version.saturating_add(1);
    state.entries.insert(
        account_id,
        CodexQuotaSchedulingEntry {
            version: state.next_version,
            revision,
            expires_at: Instant::now() + ttl,
            signals,
        },
    );
}

fn eligible_periodic_quota_refresh(account: &ProviderAccount, now: SystemTime) -> bool {
    account.enabled() && access_token_is_current(account, now)
}

fn eligible_initial_quota_sync(account: &ProviderAccount, now: SystemTime) -> bool {
    // 首次观察只兜底刚入库、尚无 quota 快照的账号
    // 已耗尽等运行时状态必须由
    // periodic 路径处理，才能保留同一账号的最小复核间隔
    account.credential_state() == CredentialState::Ready
        && eligible_periodic_quota_refresh(account, now)
}
