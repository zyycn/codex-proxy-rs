//! 请求级账号候选快照与尝试账本。

use std::collections::{BTreeSet, VecDeque};

use chrono::Utc;

use crate::fleet::pool::{
    AccountAcquireRequest, AccountCandidateLease, AccountLease, AccountPoolService,
};

/// 请求开始时冻结的完整候选集，以及候选的最终处理状态。
pub(in crate::dispatch) struct AccountAttemptLedger {
    model: String,
    candidate_ids: Vec<String>,
    pending: VecDeque<String>,
    busy: VecDeque<String>,
    attempted: BTreeSet<String>,
    state_excluded: BTreeSet<String>,
}

impl AccountAttemptLedger {
    pub(in crate::dispatch) async fn freeze(
        account_pool: &AccountPoolService,
        request: &AccountAcquireRequest,
    ) -> Self {
        let candidate_ids = account_pool.candidate_snapshot(request).await;
        Self {
            model: request.model.clone(),
            pending: candidate_ids.iter().cloned().collect(),
            candidate_ids,
            busy: VecDeque::new(),
            attempted: BTreeSet::new(),
            state_excluded: BTreeSet::new(),
        }
    }

    pub(in crate::dispatch) async fn acquire_next(
        &mut self,
        account_pool: &AccountPoolService,
    ) -> Option<AccountLease> {
        loop {
            while let Some(account_id) = self.pending.pop_front() {
                match account_pool
                    .acquire_candidate(&self.model, &account_id, Utc::now())
                    .await
                {
                    AccountCandidateLease::Acquired(lease) => {
                        self.attempted.insert(account_id);
                        return Some(*lease);
                    }
                    AccountCandidateLease::Busy => self.busy.push_back(account_id),
                    AccountCandidateLease::Unavailable => {
                        self.state_excluded.insert(account_id);
                    }
                }
            }

            if self.busy.is_empty() {
                debug_assert_eq!(
                    self.candidate_ids.len(),
                    self.attempted.len() + self.state_excluded.len()
                );
                return None;
            }

            account_pool.wait_for_candidate_change().await;
            std::mem::swap(&mut self.pending, &mut self.busy);
        }
    }

    pub(in crate::dispatch) fn candidate_count(&self) -> usize {
        self.candidate_ids.len()
    }

    pub(in crate::dispatch) fn attempted_count(&self) -> usize {
        self.attempted.len()
    }

    pub(in crate::dispatch) fn state_excluded_count(&self) -> usize {
        self.state_excluded.len()
    }
}
