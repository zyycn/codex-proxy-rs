//! 每日错峰发现与已知卡片的临期执行，周期只读取本地到期计划

use crate::{
    model::reset_credit_automation::{PlannedResetCredit, ResetCreditAutomation},
    ports::store::{AccountStore, SettingsStore},
    use_case::accounts::AccountsService,
};
use chrono::{DateTime, Utc};
use futures::StreamExt as _;
use gateway_core::{
    account::ProviderAccountId,
    task::{ScheduledTask, WorkerCycleContext, WorkerTaskError},
    time::DeploymentTimeZone,
};
use sha2::{Digest as _, Sha256};
use std::{sync::Arc, time::Duration};

/// 当地次日随机发现时间；随机数由调用者提供，结果持久化后不会因重启重抽
pub(crate) fn next_discovery(
    now: DateTime<Utc>,
    timezone: DeploymentTimeZone,
    random: u64,
) -> Option<DateTime<Utc>> {
    let start = timezone.days_after(now, 1)?;
    let end = timezone.days_after(now, 2)?;
    let seconds = u64::try_from((end - start).num_seconds()).ok()?;
    if seconds == 0 {
        return None;
    }
    start.checked_add_signed(chrono::Duration::seconds(
        i64::try_from(random % seconds).ok()?,
    ))
}

/// 提前窗口的前 10% 用于账号错峰，最多一分钟，其余时间保留给网络失败恢复
pub(crate) fn consume_at(
    account_id: &str,
    credit: &PlannedResetCredit,
    lead_minutes: u32,
) -> DateTime<Utc> {
    let lead_seconds = i64::from(lead_minutes) * 60;
    let mut hash = Sha256::new();
    hash.update(account_id.as_bytes());
    hash.update([0]);
    hash.update(credit.id.as_bytes());
    let digest = hash.finalize();
    let offset =
        i64::from(u16::from_be_bytes([digest[0], digest[1]])) % (lead_seconds / 10).clamp(1, 60);
    credit.expires_at - chrono::Duration::seconds(lead_seconds - offset)
}

pub(crate) fn next_consumption(
    record: &ResetCreditAutomation,
    lead_minutes: u32,
) -> Option<DateTime<Utc>> {
    record
        .state
        .credits
        .iter()
        .map(|credit| consume_at(&record.account_id, credit, lead_minutes))
        .min()
}

pub(crate) fn reschedule(
    record: &mut ResetCreditAutomation,
    lead_minutes: u32,
    now: DateTime<Utc>,
) {
    record.next_run_at = if let Some(retry) = record.state.retry_at {
        retry
    } else if record.state.pending.is_some() {
        now
    } else {
        next_consumption(record, lead_minutes).map_or(record.state.next_discovery_at, |due| {
            due.min(record.state.next_discovery_at)
        })
    };
}

pub(crate) struct ResetCreditAutomationTask {
    pub(crate) accounts: Arc<dyn AccountsService>,
    pub(crate) store: Arc<dyn AccountStore>,
    pub(crate) settings: Arc<dyn SettingsStore>,
}

impl ScheduledTask for ResetCreditAutomationTask {
    fn run_cycle(
        &self,
        context: WorkerCycleContext,
    ) -> futures::future::BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let settings = self
                .settings
                .load_runtime_settings()
                .await
                .map_err(|_| WorkerTaskError::safe("重置卡设置读取失败"))?;
            // 单轮有界，积压沿到期顺序继续；并发复用已有账号刷新配置
            let due = self
                .store
                .due_reset_credit_automations(Utc::now(), 100)
                .await
                .map_err(|_| WorkerTaskError::safe("重置卡计划读取失败"))?;
            let work = futures::stream::iter(due)
                .map(|account_id| async move {
                    let id = ProviderAccountId::new(account_id.clone())
                        .map_err(|_| WorkerTaskError::safe("重置卡账号 ID 不合法"))?;
                    if let Err(error) = self.accounts.reconcile_reset_credit_automation(id).await {
                        tracing::warn!(
                            account_id,
                            error_kind = error.kind().as_str(),
                            "自动重置卡任务失败"
                        );
                    }
                    Ok::<_, WorkerTaskError>(())
                })
                .buffer_unordered(settings.values.refresh_concurrency.max(1) as usize)
                .collect::<Vec<_>>();
            tokio::select! {
                () = context.cancellation().cancelled() => {},
                _ = tokio::time::timeout(Duration::from_secs(30), work) => {},
            }
            Ok(())
        })
    }
}
