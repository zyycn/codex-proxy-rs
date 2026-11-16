//! 额度桶重置后的延迟预热、使用证据复核和持久化领取

use super::*;
use chrono::{DateTime, Utc};
use gateway_core::account::QuotaWarmupStateUpdate;
use serde::{Deserialize, Serialize};

const RESET_WARMUP_DELAY_SECONDS: i64 = 5 * 60;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct BucketProgress {
    reset_at: Option<i64>,
    completed_reset_at: Option<i64>,
    last_warmup_at: Option<i64>,
    retry_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct WarmupProgress {
    #[serde(default)]
    buckets: BTreeMap<String, BucketProgress>,
}

impl WarmupProgress {
    fn reconcile(&mut self, account: &ProviderAccount, snapshot: &CodexAccountQuotaSnapshot) {
        for (key, settings) in account.quota_settings().iter() {
            if settings.warmup.is_none() {
                continue;
            }
            let Some(window) = snapshot
                .windows()
                .iter()
                .find(|window| window.is_account_wide() && window.key() == key)
            else {
                continue;
            };
            let progress = self.buckets.entry(key.to_owned()).or_default();
            let observed_reset = window.reset_at().map(|at| at.timestamp());
            if let Some(reset) = progress.reset_at {
                let last_use = snapshot
                    .last_used_at
                    .get(window.source())
                    .copied()
                    .into_iter()
                    .chain(progress.last_warmup_at)
                    .max();
                let newly_used = last_use.is_some_and(|used| used >= reset)
                    || (observed_reset.is_some_and(|next| next > reset)
                        && DateTime::<Utc>::from(snapshot.observed_at()).timestamp() >= reset
                        && window.used_percent().is_some_and(|used| used > 0.0));
                if newly_used {
                    progress.complete(reset);
                }
            }
            // 空闲桶的 resetAt 可能随查询滚动；已登记的本轮边界不能被新查询推迟
            if progress.reset_at.is_none()
                && let Some(reset) = observed_reset
                && progress
                    .completed_reset_at
                    .is_none_or(|completed| reset > completed)
            {
                progress.reset_at = Some(reset);
                progress.retry_at = None;
            }
        }
    }

    fn due(&self, account: &ProviderAccount, now: DateTime<Utc>) -> Vec<(String, String, i64)> {
        account
            .quota_settings()
            .iter()
            .filter_map(|(key, settings)| {
                let warmup = settings.warmup.as_ref()?;
                let progress = self.buckets.get(key)?;
                let reset = progress.reset_at?;
                (now.timestamp() >= reset.saturating_add(RESET_WARMUP_DELAY_SECONDS)
                    && progress
                        .retry_at
                        .is_none_or(|retry| now.timestamp() >= retry)
                    && account.model_access().allows(&warmup.model))
                .then(|| (key.to_owned(), warmup.model.clone(), reset))
            })
            .collect()
    }
}

impl BucketProgress {
    fn complete(&mut self, reset: i64) {
        self.completed_reset_at = Some(self.completed_reset_at.map_or(reset, |old| old.max(reset)));
        self.reset_at = None;
        self.retry_at = None;
    }
}

impl CodexCredentialQuotaService {
    async fn read_warmup_progress(
        &self,
        account: &ProviderAccount,
    ) -> Result<Option<(u64, WarmupProgress)>, CodexCredentialQuotaError> {
        self.store
            .load_quota_warmup_state(account)
            .await?
            .map(|stored| {
                serde_json::from_value(Value::Object(stored.data))
                    .map(|progress| (stored.version, progress))
                    .map_err(|_| CodexCredentialQuotaError::InvalidCredentialData)
            })
            .transpose()
    }

    async fn write_warmup_progress(
        &self,
        account: &ProviderAccount,
        snapshot: &CodexAccountQuotaSnapshot,
        version: u64,
        progress: &WarmupProgress,
    ) -> Result<bool, CodexCredentialQuotaError> {
        let data = serde_json::to_value(progress)
            .map_err(|_| CodexCredentialQuotaError::InvalidCredentialData)?;
        Ok(self
            .store
            .compare_and_swap_quota_warmup_state(QuotaWarmupStateUpdate {
                account,
                expected_version: version,
                expected_quota_observed_at: Some(snapshot.observed_at()),
                data: data
                    .as_object()
                    .cloned()
                    .ok_or(CodexCredentialQuotaError::InvalidCredentialData)?,
            })
            .await?)
    }

    /// 与 Cron 共用同一受租约保护的 worker，每次周期独立于全局 Cron 开关
    pub async fn execute_bucket_warmup_at(
        &self,
        now: DateTime<Utc>,
    ) -> Result<CodexWarmupSummary, CodexCredentialQuotaError> {
        let mut summary = CodexWarmupSummary::default();
        for account in self.repository.list_for_provider().await? {
            if !warmup::warmup_account_is_available(&account)
                || !account.quota_settings().has_warmup()
            {
                continue;
            }
            match self.warmup_buckets_for_account(&account, now).await {
                Ok(Some(true)) => summary.warmed_up += 1,
                Ok(Some(false)) => summary.failed += 1,
                Ok(None) => {}
                Err(error) => {
                    summary.failed += 1;
                    tracing::warn!(account_id = %account.id(), error = %error, "OpenAI quota bucket warmup check failed");
                }
            }
        }
        Ok(summary)
    }

    async fn warmup_buckets_for_account(
        &self,
        account: &ProviderAccount,
        now: DateTime<Utc>,
    ) -> Result<Option<bool>, CodexCredentialQuotaError> {
        let Some(snapshot) = self.read_snapshot_for(account).await? else {
            return Ok(None);
        };
        let Some((version, mut progress)) = self.read_warmup_progress(account).await? else {
            return Ok(None);
        };
        let previous = progress.clone();
        progress.reconcile(account, &snapshot);
        let due = progress.due(account, now);
        // 刷新失败或账号仍不可用时最多每五分钟复查，普通轮询不反复访问上游
        for (key, _, _) in &due {
            progress
                .buckets
                .get_mut(key)
                .expect("已登记额度桶")
                .retry_at = Some(now.timestamp() + RESET_WARMUP_DELAY_SECONDS);
        }
        if progress != previous
            && !self
                .write_warmup_progress(account, &snapshot, version, &progress)
                .await?
        {
            return Ok(None);
        }
        if due.is_empty() {
            return Ok(None);
        }
        self.refresh_account(account.id()).await?;
        let Some(current) = self.store.get_account(account.id()).await? else {
            return Ok(None);
        };
        if !warmup::warmup_account_is_available(&current)
            || current.quota().is_exhausted()
            || current.quota_paused()
            || !access_token_is_current(&current, SystemTime::now())
        {
            return Ok(None);
        }
        let Some(snapshot) = self.read_snapshot_for(&current).await? else {
            return Ok(None);
        };
        let Some((version, mut progress)) = self.read_warmup_progress(&current).await? else {
            return Ok(None);
        };
        progress.reconcile(&current, &snapshot);
        let selected = due.iter().find(|(key, model, reset)| {
            current
                .quota_settings()
                .iter()
                .any(|(current_key, settings)| {
                    current_key == key
                        && settings
                            .warmup
                            .as_ref()
                            .is_some_and(|warmup| warmup.model == *model)
                })
                && progress
                    .buckets
                    .get(key)
                    .is_some_and(|bucket| bucket.reset_at == Some(*reset))
                && snapshot.windows().iter().any(|window| {
                    window.is_account_wide() && window.key() == key && !window.limit_reached()
                })
                && current.model_access().allows(model)
        });
        let Some((_, model, _)) = selected else {
            self.write_warmup_progress(&current, &snapshot, version, &progress)
                .await?;
            return Ok(None);
        };
        let profile = self
            .profile
            .resolve_global_request_profile(self.runtime_policy.as_ref())
            .await
            .map_err(CodexCredentialQuotaError::Profile)?;
        let client = CodexBackendClient::new(
            self.http.clone(),
            self.base_url.clone(),
            self.profile.clone(),
        )
        .with_request_profile(profile);
        // 先持久化领取再发请求；超时、取消或重启后不重放结果不明的推理
        // 通用窗口共享请求用量，同批到期窗口只发一次轻量请求
        for (key, _, reset) in &due {
            if let Some(bucket) = progress.buckets.get_mut(key)
                && bucket.reset_at == Some(*reset)
            {
                bucket.complete(*reset);
            }
        }
        if !self
            .write_warmup_progress(&current, &snapshot, version, &progress)
            .await?
        {
            return Ok(None);
        }
        let succeeded = self.warmup_one(&current, model, &client).await;
        if succeeded {
            self.record_scheduled_warmup_use(&current, Utc::now())
                .await?;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        Ok(Some(succeeded))
    }

    pub(super) async fn record_scheduled_warmup_use(
        &self,
        account: &ProviderAccount,
        now: DateTime<Utc>,
    ) -> Result<(), CodexCredentialQuotaError> {
        if !account.quota_settings().has_warmup() {
            return Ok(());
        }
        for _ in 0..QUOTA_MERGE_ATTEMPTS {
            let Some(snapshot) = self.read_snapshot_for(account).await? else {
                return Ok(());
            };
            let Some((version, mut progress)) = self.read_warmup_progress(account).await? else {
                return Ok(());
            };
            progress.reconcile(account, &snapshot);
            for (key, settings) in account.quota_settings().iter() {
                if settings.warmup.is_some()
                    && snapshot
                        .windows()
                        .iter()
                        .any(|window| window.is_account_wide() && window.key() == key)
                {
                    progress
                        .buckets
                        .entry(key.to_owned())
                        .or_default()
                        .last_warmup_at = Some(now.timestamp());
                }
            }
            progress.reconcile(account, &snapshot);
            if self
                .write_warmup_progress(account, &snapshot, version, &progress)
                .await?
            {
                break;
            }
        }
        Ok(())
    }
}
