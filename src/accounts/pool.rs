use std::collections::{BTreeMap, VecDeque};

use chrono::{DateTime, Duration, Utc};

use crate::accounts::model::{Account, AccountStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationStrategy {
    LeastUsed,
    RoundRobin,
    Sticky,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountPoolOptions {
    pub max_concurrent_per_account: usize,
    pub stale_slot_ttl: Duration,
    pub rotation_strategy: RotationStrategy,
    pub skip_quota_limited: bool,
    pub tier_priority: Vec<String>,
    pub model_plan_allowlist: BTreeMap<String, Vec<String>>,
}

impl Default for AccountPoolOptions {
    fn default() -> Self {
        Self {
            max_concurrent_per_account: 3,
            stale_slot_ttl: Duration::minutes(5),
            rotation_strategy: RotationStrategy::LeastUsed,
            skip_quota_limited: true,
            tier_priority: Vec::new(),
            model_plan_allowlist: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountAcquireRequest {
    pub model: String,
    pub exclude_account_ids: Vec<String>,
    pub preferred_account_id: Option<String>,
    pub now: DateTime<Utc>,
}

impl AccountAcquireRequest {
    pub fn new(model: impl Into<String>, now: DateTime<Utc>) -> Self {
        Self {
            model: model.into(),
            exclude_account_ids: Vec::new(),
            preferred_account_id: None,
            now,
        }
    }

    pub fn with_exclude_account_ids(
        mut self,
        account_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.exclude_account_ids = account_ids.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_preferred_account_id(mut self, account_id: impl Into<String>) -> Self {
        self.preferred_account_id = Some(account_id.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct AcquiredAccount {
    pub account: Account,
    pub previous_slot_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountCapacitySummary {
    pub max_concurrent_per_account: usize,
    pub total_slots: usize,
    pub used_slots: usize,
    pub available_slots: usize,
}

#[derive(Debug)]
pub struct AccountPool {
    accounts: BTreeMap<String, Account>,
    slots: BTreeMap<String, VecDeque<DateTime<Utc>>>,
    options: AccountPoolOptions,
    round_robin_cursor: usize,
}

impl Default for AccountPool {
    fn default() -> Self {
        Self::with_options(AccountPoolOptions::default())
    }
}

impl AccountPool {
    pub fn with_options(options: AccountPoolOptions) -> Self {
        Self {
            accounts: BTreeMap::new(),
            slots: BTreeMap::new(),
            options,
            round_robin_cursor: 0,
        }
    }

    pub fn insert(&mut self, account: Account) {
        self.accounts.insert(account.id.clone(), account);
    }

    pub fn set_status(&mut self, account_id: &str, status: AccountStatus) -> bool {
        let Some(account) = self.accounts.get_mut(account_id) else {
            return false;
        };
        account.status = status;
        if status != AccountStatus::Active {
            self.slots.remove(account_id);
        }
        true
    }

    pub fn mark_quota_limited_until(
        &mut self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> bool {
        let Some(account) = self.accounts.get_mut(account_id) else {
            return false;
        };
        account.quota_limit_reached = true;
        account.quota_cooldown_until = Some(cooldown_until);
        self.slots.remove(account_id);
        true
    }

    pub fn set_cloudflare_cooldown_until(
        &mut self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> bool {
        let Some(account) = self.accounts.get_mut(account_id) else {
            return false;
        };
        account.cloudflare_cooldown_until = Some(cooldown_until);
        self.slots.remove(account_id);
        true
    }

    pub fn acquire(&mut self, model: &str) -> Option<Account> {
        self.acquire_with(AccountAcquireRequest::new(model, Utc::now()))
            .map(|acquired| acquired.account)
    }

    pub fn acquire_with(&mut self, request: AccountAcquireRequest) -> Option<AcquiredAccount> {
        self.cleanup_stale_slots(request.now);
        let candidates = self.candidates(&request);
        let selected = if let Some(preferred_account_id) = &request.preferred_account_id {
            candidates
                .iter()
                .find(|account| account.id == *preferred_account_id)
                .cloned()
        } else {
            None
        }
        .or_else(|| match self.options.rotation_strategy {
            RotationStrategy::LeastUsed => self.select_least_used(&candidates),
            RotationStrategy::RoundRobin => self.select_round_robin(&candidates),
            RotationStrategy::Sticky => self.select_sticky(&candidates),
        })?;
        let previous_slot_at = self.previous_slot_at(&selected.id);
        self.push_slot(&selected.id, request.now);
        Some(AcquiredAccount {
            account: selected,
            previous_slot_at,
        })
    }

    pub fn release(&mut self, account_id: &str) {
        let Some(slots) = self.slots.get_mut(account_id) else {
            return;
        };
        slots.pop_front();
        if slots.is_empty() {
            self.slots.remove(account_id);
        }
    }

    pub fn capacity_summary(&mut self, now: DateTime<Utc>) -> AccountCapacitySummary {
        self.cleanup_stale_slots(now);
        let active_accounts = self
            .accounts
            .values()
            .filter(|account| {
                account.status == AccountStatus::Active && self.is_quota_available(account, now)
            })
            .count();
        let total_slots = active_accounts * self.options.max_concurrent_per_account;
        let used_slots = self
            .slots
            .iter()
            .filter(|(account_id, _)| {
                self.accounts
                    .get(*account_id)
                    .is_some_and(|account| account.status == AccountStatus::Active)
            })
            .map(|(_, slots)| slots.len().min(self.options.max_concurrent_per_account))
            .sum();

        AccountCapacitySummary {
            max_concurrent_per_account: self.options.max_concurrent_per_account,
            total_slots,
            used_slots,
            available_slots: total_slots.saturating_sub(used_slots),
        }
    }

    fn select_least_used(&mut self, candidates: &[Account]) -> Option<Account> {
        let best_last_used = candidates
            .iter()
            .map(|account| account.last_used_at.as_deref())
            .min()?;
        let tied = candidates
            .iter()
            .filter(|account| account.last_used_at.as_deref() == best_last_used)
            .collect::<Vec<_>>();
        let index = self.round_robin_cursor % tied.len();
        self.round_robin_cursor = (self.round_robin_cursor + 1) % tied.len();
        Some((*tied[index]).clone())
    }

    fn select_sticky(&self, candidates: &[Account]) -> Option<Account> {
        candidates
            .iter()
            .max_by_key(|account| account.last_used_at.clone())
            .cloned()
    }

    fn select_round_robin(&mut self, candidates: &[Account]) -> Option<Account> {
        if candidates.is_empty() {
            return None;
        }
        let index = self.round_robin_cursor % candidates.len();
        self.round_robin_cursor = (index + 1) % candidates.len();
        Some(candidates[index].clone())
    }

    fn candidates(&self, request: &AccountAcquireRequest) -> Vec<Account> {
        let mut candidates = self
            .accounts
            .values()
            .filter(|account| self.is_base_available(account, request))
            .cloned()
            .collect::<Vec<_>>();
        if let Some(best_tier) = self.best_available_tier(&candidates) {
            candidates.retain(|account| account.plan_type.as_deref() == Some(best_tier.as_str()));
        }
        candidates
    }

    fn is_base_available(&self, account: &Account, request: &AccountAcquireRequest) -> bool {
        account.status == AccountStatus::Active
            && self.slot_count(&account.id) < self.options.max_concurrent_per_account
            && self.is_quota_available(account, request.now)
            && self.is_model_allowed(account, &request.model)
            && !request
                .exclude_account_ids
                .iter()
                .any(|account_id| account_id == &account.id)
            && account
                .cloudflare_cooldown_until
                .is_none_or(|cooldown_until| request.now >= cooldown_until)
    }

    fn is_quota_available(&self, account: &Account, now: DateTime<Utc>) -> bool {
        if !self.options.skip_quota_limited || !account.quota_limit_reached {
            return true;
        }
        account
            .quota_cooldown_until
            .is_some_and(|cooldown_until| now >= cooldown_until)
    }

    fn slot_count(&self, account_id: &str) -> usize {
        self.slots.get(account_id).map_or(0, VecDeque::len)
    }

    fn previous_slot_at(&self, account_id: &str) -> Option<DateTime<Utc>> {
        self.slots
            .get(account_id)
            .and_then(|slots| slots.back().cloned())
    }

    fn push_slot(&mut self, account_id: &str, now: DateTime<Utc>) {
        self.slots
            .entry(account_id.to_string())
            .or_default()
            .push_back(now);
    }

    fn cleanup_stale_slots(&mut self, now: DateTime<Utc>) {
        let ttl = self.options.stale_slot_ttl;
        self.slots.retain(|_, slots| {
            // slot 只代表本进程内的在途请求，超过 TTL 后必须释放，避免异常中断永久占满账号。
            slots.retain(|slot_at| now.signed_duration_since(*slot_at) <= ttl);
            !slots.is_empty()
        });
    }

    fn is_model_allowed(&self, account: &Account, model: &str) -> bool {
        let Some(allowed_plans) = self.options.model_plan_allowlist.get(model) else {
            return true;
        };
        allowed_plans
            .iter()
            .any(|plan| account.plan_type.as_deref() == Some(plan.as_str()))
    }

    fn best_available_tier(&self, candidates: &[Account]) -> Option<String> {
        self.options.tier_priority.iter().find_map(|tier| {
            candidates
                .iter()
                .find(|account| account.plan_type.as_deref() == Some(tier.as_str()))
                .and_then(|account| account.plan_type.clone())
        })
    }
}
