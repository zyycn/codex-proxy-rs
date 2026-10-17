//! 重置卡自动使用的账号锁、持久幂等键、查询缓存与重试编排

use super::*;

impl DefaultAccountsService {
    pub(super) async fn reset_credits_upstream(
        &self,
        context: &MutationContext,
        account_id: ProviderAccountId,
    ) -> Result<ProviderResetCredits, AdminError> {
        let (_, provider) = self.provider_for_account(&account_id).await?;
        match provider.reset_credits(&account_id).await {
            Ok(credits) => Ok(credits),
            Err(error)
                if error.kind()
                    == crate::ports::provider::ProviderAdminErrorKind::CredentialRefreshRequired =>
            {
                self.refresh(context, account_id.clone()).await?;
                let (_, provider) = self.provider_for_account(&account_id).await?;
                provider
                    .reset_credits(&account_id)
                    .await
                    .map_err(map_reset_credits_error_after_refresh)
            }
            Err(error) => Err(map_provider_error(error, "provider reset credits")),
        }
    }

    pub(super) async fn consume_reset_credit_upstream(
        &self,
        context: &MutationContext,
        command: ConsumeProviderResetCredit,
    ) -> Result<ProviderResetCreditResult, AdminError> {
        let account_id = command.account_id.clone();
        let (_, provider) = self.provider_for_account(&account_id).await?;
        match provider.consume_reset_credit(command.clone()).await {
            Ok(result) => Ok(result),
            Err(error)
                if error.kind()
                    == crate::ports::provider::ProviderAdminErrorKind::CredentialRefreshRequired =>
            {
                self.refresh(context, account_id.clone()).await?;
                let (_, provider) = self.provider_for_account(&account_id).await?;
                provider
                    .consume_reset_credit(command)
                    .await
                    .map_err(map_reset_credits_error_after_refresh)
            }
            Err(error) => Err(map_provider_error(error, "provider reset-credit consume")),
        }
    }
}

use crate::{
    model::{MutationActor, reset_credit_automation::*},
    reset_credit_automation::{consume_at, next_consumption, next_discovery, reschedule},
};
use chrono::{DateTime, Utc};
use rand_core::{OsRng, RngCore as _};

impl DefaultAccountsService {
    async fn automation_lead_minutes(&self) -> Result<u32, AdminError> {
        Ok(self
            .settings
            .load_runtime_settings()
            .await
            .map_err(|e| map_store_error(e, "runtime settings"))?
            .values
            .reset_credit_lead_minutes)
    }

    pub(super) async fn automation_status(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<ResetCreditAutomationStatus, AdminError> {
        self.require_reset_credit_automation(account_id).await?;
        let lead_minutes = self.automation_lead_minutes().await?;
        let record = self
            .accounts
            .reset_credit_automation(account_id.as_str())
            .await
            .map_err(|e| map_store_error(e, "reset-credit automation"))?;
        Ok(ResetCreditAutomationStatus {
            enabled: record.as_ref().is_some_and(|r| r.enabled),
            lead_minutes,
            next_discovery_at: record
                .as_ref()
                .filter(|r| r.enabled)
                .map(|r| r.state.next_discovery_at),
            next_consume_at: record.as_ref().filter(|r| r.enabled).and_then(|r| {
                next_consumption(r, lead_minutes)
                    .or_else(|| {
                        r.state
                            .pending
                            .as_ref()
                            .map(|pending| consume_at(&r.account_id, &pending.credit, lead_minutes))
                    })
                    .map(|due| r.state.retry_at.map_or(due, |retry| due.max(retry)))
            }),
            last_result: record.and_then(|r| {
                if r.state
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.credit.expires_at <= Utc::now())
                {
                    Some("expired_unknown".to_owned())
                } else {
                    r.state.last_result
                }
            }),
        })
    }

    async fn require_reset_credit_automation(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<bool, AdminError> {
        let (stored, provider) = self.provider_for_account(account_id).await?;
        let capabilities =
            provider.account_capabilities(account_id, &stored.account.authentication_kind);
        if !capabilities.reset_credits || !capabilities.consume_reset_credit {
            return Err(AdminError::invalid("当前账号不支持自动重置卡"));
        }
        Ok(stored.account.enabled)
    }

    pub(super) async fn configure_automation(
        &self,
        context: &MutationContext,
        account_id: &ProviderAccountId,
        enabled: bool,
    ) -> Result<ResetCreditAutomationStatus, AdminError> {
        let lock = self.reset_credit_lock(account_id).await;
        let _guard = lock.lock().await;
        self.require_reset_credit_automation(account_id).await?;
        let now = Utc::now();
        let mut record = self
            .accounts
            .reset_credit_automation(account_id.as_str())
            .await
            .map_err(|e| map_store_error(e, "reset-credit automation"))?
            .unwrap_or_else(|| ResetCreditAutomation {
                account_id: account_id.as_str().to_owned(),
                enabled: false,
                revision: 0,
                next_run_at: now,
                state: ResetCreditAutomationState {
                    next_discovery_at: now,
                    credits: vec![],
                    pending: None,
                    retry_at: None,
                    failures: 0,
                    last_result: None,
                },
            });
        if record.enabled != enabled {
            record.enabled = enabled;
            // 首次开启错开一分钟；重新开启保留已知到期时间及未决幂等键
            if enabled && record.revision == 0 {
                record.state.next_discovery_at =
                    now + chrono::Duration::seconds(i64::from(OsRng.next_u32() % 60));
            }
            let lead = self.automation_lead_minutes().await?;
            reschedule(&mut record, lead, now);
            self.save_automation(&mut record, Some(context)).await?;
            let settings = self
                .settings
                .load_runtime_settings()
                .await
                .map_err(|e| map_store_error(e, "runtime settings"))?;
            publish_committed(self.snapshot.as_ref(), settings.config_revision).await?;
        }
        self.automation_status(account_id).await
    }

    async fn save_automation(
        &self,
        record: &mut ResetCreditAutomation,
        context: Option<&MutationContext>,
    ) -> Result<(), AdminError> {
        record.revision = self
            .accounts
            .save_reset_credit_automation(record, context)
            .await
            .map_err(|e| map_store_error(e, "reset-credit automation"))?;
        Ok(())
    }

    fn observe_reset_credits(
        &self,
        record: &mut ResetCreditAutomation,
        credits: &ProviderResetCredits,
        now: DateTime<Utc>,
    ) -> Result<(), AdminError> {
        record.state.credits = credits
            .credits
            .iter()
            .filter(|credit| credit.status.as_deref() == Some("available"))
            .filter_map(|credit| {
                credit
                    .expires_at
                    .filter(|expiry| *expiry > now)
                    .map(|expires_at| PlannedResetCredit {
                        id: credit.id.clone(),
                        expires_at,
                    })
            })
            .collect();
        record.state.credits.sort_by_key(|credit| credit.expires_at);
        record.state.credits.dedup_by(|a, b| a.id == b.id);
        if record.state.next_discovery_at <= now
            || self
                .timezone
                .local(record.state.next_discovery_at)
                .date_naive()
                == self.timezone.local(now).date_naive()
        {
            record.state.next_discovery_at =
                next_discovery(now, self.timezone, OsRng.next_u64())
                    .ok_or_else(|| AdminError::internal("无法计算重置卡发现时间"))?;
        }
        if record.state.pending.is_none() {
            record.state.retry_at = None;
            record.state.failures = 0;
        }
        Ok(())
    }

    pub(super) async fn remember_reset_credits(
        &self,
        account_id: &ProviderAccountId,
        credits: &ProviderResetCredits,
    ) -> Result<(), AdminError> {
        let Some(mut record) = self
            .accounts
            .reset_credit_automation(account_id.as_str())
            .await
            .map_err(|e| map_store_error(e, "reset-credit automation"))?
        else {
            return Ok(());
        };
        if !record.enabled {
            return Ok(());
        }
        let now = Utc::now();
        self.observe_reset_credits(&mut record, credits, now)?;
        reschedule(&mut record, self.automation_lead_minutes().await?, now);
        self.save_automation(&mut record, None).await
    }

    async fn automation_failed(
        &self,
        record: &mut ResetCreditAutomation,
        lead: u32,
        now: DateTime<Utc>,
    ) -> Result<(), AdminError> {
        if record.state.pending.is_none()
            && !next_consumption(record, lead).is_some_and(|due| due <= now)
        {
            record.state.next_discovery_at =
                next_discovery(now, self.timezone, OsRng.next_u64())
                    .ok_or_else(|| AdminError::internal("无法计算重置卡发现时间"))?;
            record.state.retry_at = None;
            record.state.last_result = Some("discovery_failed".to_owned());
            reschedule(record, lead, now);
            return self.save_automation(record, None).await;
        }
        record.state.failures = record.state.failures.saturating_add(1);
        let delay = (30_i64 * 2_i64.pow(record.state.failures.min(4) - 1)).min(300)
            + i64::from(OsRng.next_u32() % 30);
        record.state.retry_at = Some(now + chrono::Duration::seconds(delay));
        record.state.last_result = Some(
            if record.state.pending.is_some() {
                "pending"
            } else {
                "discovery_failed"
            }
            .to_owned(),
        );
        reschedule(record, lead, now);
        self.save_automation(record, None).await
    }

    pub(super) async fn reconcile_automation(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<(), AdminError> {
        let lock = self.reset_credit_lock(account_id).await;
        let _guard = lock.lock().await;
        let Some(mut record) = self
            .accounts
            .reset_credit_automation(account_id.as_str())
            .await
            .map_err(|e| map_store_error(e, "reset-credit automation"))?
        else {
            return Ok(());
        };
        if !record.enabled {
            return Ok(());
        }
        match self.require_reset_credit_automation(account_id).await {
            Ok(true) => {}
            Ok(false) => return Ok(()),
            Err(error) => {
                record.next_run_at = next_discovery(Utc::now(), self.timezone, OsRng.next_u64())
                    .ok_or_else(|| AdminError::internal("无法计算重置卡发现时间"))?;
                self.save_automation(&mut record, None).await?;
                return Err(error);
            }
        }
        let now = Utc::now();
        let lead = self.automation_lead_minutes().await?;
        record
            .state
            .credits
            .retain(|credit| credit.expires_at > now);
        if record
            .state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.credit.expires_at <= now)
        {
            record.state.pending = None;
            record.state.retry_at = None;
            record.state.last_result = Some("expired_unknown".to_owned());
        }
        if record.state.retry_at.is_some_and(|retry| retry > now) {
            reschedule(&mut record, lead, now);
            return self.save_automation(&mut record, None).await;
        }
        let context = MutationContext {
            actor: MutationActor::System,
            request_id: "reset-credit-automation".to_owned(),
        };
        let due_credit = next_consumption(&record, lead).is_some_and(|due| due <= now);
        if record.state.pending.is_none()
            && (due_credit
                || record.state.next_discovery_at <= now
                || record.state.retry_at.is_some())
        {
            // 先保存下一次发现，查询被取消或进程重启也不会重复刷普通发现接口
            if record.state.next_discovery_at <= now {
                record.state.next_discovery_at =
                    next_discovery(now, self.timezone, OsRng.next_u64())
                        .ok_or_else(|| AdminError::internal("无法计算重置卡发现时间"))?;
            }
            if due_credit || record.state.retry_at.is_some() {
                record.state.retry_at = Some(now + chrono::Duration::seconds(30));
            }
            reschedule(&mut record, lead, now);
            self.save_automation(&mut record, None).await?;
            // 真正消费前重新核对卡片；每日发现结果也可直接承担本次核对
            match self
                .reset_credits_upstream(&context, account_id.clone())
                .await
            {
                Ok(credits) => self.observe_reset_credits(&mut record, &credits, Utc::now())?,
                Err(error) => {
                    self.automation_failed(&mut record, lead, Utc::now())
                        .await?;
                    return Err(error);
                }
            }
            if let Some(credit) = record
                .state
                .credits
                .iter()
                .find(|credit| consume_at(account_id.as_str(), credit, lead) <= Utc::now())
                .cloned()
            {
                record.state.pending = Some(PendingResetCredit {
                    credit,
                    redeem_request_id: uuid::Uuid::new_v4().to_string(),
                });
                record.state.last_result = Some("pending".to_owned());
                record.state.retry_at = Some(Utc::now() + chrono::Duration::seconds(30));
                reschedule(&mut record, lead, Utc::now());
                // 必须在发送消费请求前落盘，进程退出或响应丢失继续使用相同幂等键
                self.save_automation(&mut record, None).await?;
            }
        }
        let Some(pending) = record.state.pending.clone() else {
            reschedule(&mut record, lead, Utc::now());
            return self.save_automation(&mut record, None).await;
        };
        let command = ConsumeProviderResetCredit {
            account_id: account_id.clone(),
            credit_id: Some(pending.credit.id.clone()),
            redeem_request_id: uuid::Uuid::parse_str(&pending.redeem_request_id)
                .map_err(|_| AdminError::internal("重置卡幂等键不合法"))?,
        };
        match self.consume_reset_credit_upstream(&context, command).await {
            Ok(result) => {
                let confirmed = matches!(result.code.as_str(), "reset" | "already_redeemed");
                record.state.pending = None;
                record
                    .state
                    .credits
                    .retain(|credit| credit.id != pending.credit.id);
                record.state.retry_at = None;
                record.state.failures = 0;
                record.state.last_result = Some(if confirmed {
                    result.code
                } else {
                    "not_redeemed".to_owned()
                });
                reschedule(&mut record, lead, Utc::now());
                // 已确认的结果先落盘，再刷新额度；观测失败不能重放已完成的消费
                self.save_automation(&mut record, None).await?;
                if confirmed {
                    let _ = self.quota(account_id, true).await;
                }
            }
            Err(error) => {
                self.automation_failed(&mut record, lead, Utc::now())
                    .await?;
                return Err(error);
            }
        }
        Ok(())
    }
}
