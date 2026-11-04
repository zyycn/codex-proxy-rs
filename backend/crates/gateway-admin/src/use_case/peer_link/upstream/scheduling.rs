//! 将互联观测压缩为按模型索引的调度事实，保留各层独立的有效期

use std::{collections::HashMap, sync::Arc, time::SystemTime};

use gateway_protocol::peer::{PeerComponentStatus, PeerLink, PeerModelCoverage, PeerModelState};

use super::{quota, snapshot};

use gateway_core::account::{QuotaAccessState, UpstreamSchedulingSignal};

#[derive(Clone, Copy)]
struct Validity {
    observed_at: SystemTime,
    expires_at: SystemTime,
}

impl Validity {
    fn component(observed: Option<&str>, expires: Option<&str>) -> Option<Self> {
        let observed_at = observed.and_then(snapshot::timestamp)?;
        let expires_at = match expires {
            Some(value) => snapshot::timestamp(value)?,
            None => observed_at + snapshot::FRESH_FOR,
        };
        (expires_at <= observed_at + snapshot::MAX_AGE).then_some(Self {
            observed_at,
            expires_at,
        })
    }

    fn fresh(self, now: SystemTime) -> bool {
        self.observed_at <= now + std::time::Duration::from_secs(60) && self.expires_at > now
    }
}

struct QuotaWindow {
    remaining_percent: f64,
    reset_at: Option<SystemTime>,
}

struct AccountQuota {
    validity: Validity,
    windows: Vec<QuotaWindow>,
}

impl AccountQuota {
    fn remaining_percent(&self, now: SystemTime) -> Option<f64> {
        if !self.validity.fresh(now) {
            return None;
        }
        self.windows
            .iter()
            .filter(|window| window.reset_at.is_none_or(|reset| reset > now))
            .map(|window| window.remaining_percent)
            .reduce(f64::min)
    }
}

struct ModelObservation {
    validity: Validity,
    exhausted: bool,
    account: Option<Arc<AccountQuota>>,
}

pub(super) struct SchedulingObservation {
    validity: Validity,
    concurrency_limit: Option<gateway_core::account::AccountConcurrencyLimit>,
    exhausted_budgets: Vec<Validity>,
    models: HashMap<String, Option<ModelObservation>>,
}

impl SchedulingObservation {
    pub(super) fn new(peer: &PeerLink, now: SystemTime) -> Option<Self> {
        let validity = Validity::component(Some(&peer.observed_at), Some(&peer.expires_at))?;
        let mut exhausted_budgets = Vec::new();
        if let Some(component) = &peer.usage
            && component.status == PeerComponentStatus::Available
            && let Some(usage) = &component.data
            && let Some(usage_validity) =
                Validity::component(component.observed_at.as_deref(), None)
            && quota::access(usage, now) == QuotaAccessState::Exhausted
        {
            if usage.allowed.is_some() {
                exhausted_budgets.push(usage_validity);
            } else {
                // 窗口提供的补充证据只在各自重置边界之前影响调度评分
                for window in &usage.windows {
                    if quota::window_access(window, now) == QuotaAccessState::Exhausted {
                        let mut validity = usage_validity;
                        if let Some(reset) =
                            window.reset_at.as_deref().and_then(snapshot::timestamp)
                        {
                            validity.expires_at = validity.expires_at.min(reset);
                        }
                        exhausted_budgets.push(validity);
                    }
                }
            }
        }

        // 同一账号重复列出模型不增加候选数，多账号覆盖不计算池平均百分比
        let mut model_accounts = HashMap::<&str, Option<usize>>::new();
        let quota_accounts = peer
            .quota
            .as_ref()
            .map_or(&[][..], |quota| quota.accounts.as_slice());
        for (index, account) in quota_accounts.iter().enumerate() {
            for model in &account.models {
                model_accounts
                    .entry(model)
                    .and_modify(|candidate| {
                        if *candidate != Some(index) {
                            *candidate = None;
                        }
                    })
                    .or_insert(Some(index));
            }
        }
        let mut accounts = HashMap::<usize, Option<Arc<AccountQuota>>>::new();
        let mut models = HashMap::with_capacity(peer.models.len());
        for model in &peer.models {
            models.entry(model.model.clone()).or_insert_with(|| {
                if model.coverage != PeerModelCoverage::Complete
                    || model.state == PeerModelState::Unknown
                {
                    return None;
                }
                let validity =
                    Validity::component(model.observed_at.as_deref(), model.expires_at.as_deref())?;
                let account = model_accounts
                    .get(model.model.as_str())
                    .copied()
                    .flatten()
                    .and_then(|index| {
                        accounts
                            .entry(index)
                            .or_insert_with(|| {
                                let account = &quota_accounts[index];
                                let validity = Validity::component(
                                    account.observed_at.as_deref(),
                                    account.expires_at.as_deref(),
                                )?;
                                Some(Arc::new(AccountQuota {
                                    validity,
                                    windows: account
                                        .windows
                                        .iter()
                                        .filter_map(|window| {
                                            Some(QuotaWindow {
                                                remaining_percent: 100.0 - window.used_percent?,
                                                reset_at: window
                                                    .reset_at
                                                    .as_deref()
                                                    .and_then(snapshot::timestamp),
                                            })
                                        })
                                        .collect(),
                                }))
                            })
                            .clone()
                    });
                Some(ModelObservation {
                    validity,
                    exhausted: model.state == PeerModelState::Exhausted,
                    account,
                })
            });
        }
        Some(Self {
            validity,
            concurrency_limit: peer
                .key_limits
                .and_then(|limits| limits.max_concurrency)
                .and_then(gateway_core::account::AccountConcurrencyLimit::from_key_limit),
            exhausted_budgets,
            models,
        })
    }

    pub(super) fn fresh(&self, now: SystemTime) -> bool {
        self.validity.fresh(now) && snapshot::fresh_at(self.validity.observed_at, now)
    }

    pub(super) fn concurrency_limit(
        &self,
        now: SystemTime,
    ) -> Option<gateway_core::account::AccountConcurrencyLimit> {
        self.concurrency_limit.filter(|_| self.fresh(now))
    }

    pub(super) fn signal(
        &self,
        model: Option<&str>,
        now: SystemTime,
    ) -> Option<UpstreamSchedulingSignal> {
        if !self.fresh(now) {
            return None;
        }
        let budget_exhausted = self
            .exhausted_budgets
            .iter()
            .any(|validity| validity.fresh(now));
        let model = model
            .and_then(|id| self.models.get(id)?.as_ref())
            .filter(|model| model.validity.fresh(now));
        if !budget_exhausted && model.is_none() && self.concurrency_limit.is_none() {
            return None;
        }
        Some(UpstreamSchedulingSignal {
            concurrency_limit: self.concurrency_limit,
            exhausted: budget_exhausted || model.is_some_and(|model| model.exhausted),
            remaining_percent: if budget_exhausted {
                None
            } else {
                model.and_then(|model| model.account.as_ref()?.remaining_percent(now))
            },
        })
    }
}
