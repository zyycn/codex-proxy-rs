//! Codex AT/RT 刷新状态机；Redis lease + ProviderAccountStore CAS，无 SQL。

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use futures::{StreamExt as _, stream};
use gateway_core::engine::credential::{AccountAvailability, ProviderAccount, ProviderAccountId};
use gateway_core::provider_ports::{
    ProviderCredentialStatePort, ProviderLeaseAcquisition, ProviderLeasePort, ProviderLeaseRequest,
    ProviderRefreshCapacityRequest, ProviderRefreshLeaseRequest, ProviderRefreshPolicy,
    ProviderRuntimePolicyPort, ProviderStoreError, provider_refresh_backoff_at,
    provider_refresh_retry_at,
};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;

use super::recovery_log::{CodexOAuthRecoveryOperation, record_oauth_recovery};
use super::repository::{CodexCredentialRepository, CredentialRepositoryError};
use super::token_client::{RefreshFailure, TokenPair, TokenRefresher};
use super::types::CodexOAuthSecret;

const PROVIDER_NAME: &str = "openai";
const MAX_REFRESH_BATCH: u32 = 1_000;
/// 连续失败计数窗口；每次瞬态失败刷新该 TTL，静默满窗后计数过期归零。
const REFRESH_BACKOFF_WINDOW: Duration = Duration::from_secs(30 * 60);
/// 窗口内先执行五次指数退避，再进入固定恢复周期。
const REFRESH_BACKOFF_MAX_ATTEMPTS: u32 = 5;
/// 耗尽指数退避后的 OAuth 恢复周期。
const REFRESH_RECOVERY_DELAY: Duration = Duration::from_secs(10 * 60);
/// 过期 AT 仍允许 RT 恢复的最长窗口。
const REFRESH_RECOVERY_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
/// 首次 OAuth 入库最多提前五分钟刷新，避免一小时 token 配合一小时安全边际立刻消耗 RT。
const INITIAL_OAUTH_REFRESH_MAX_MARGIN: Duration = Duration::from_secs(5 * 60);

/// 计算首次 OAuth account 写入时的刷新时间。
///
/// 首次 code exchange 已经拿到一组新的 AT/RT；即使全局安全边际大于等于 AT
/// 生命周期，也不能让它立刻进入 RT 刷新队列。后续刷新仍使用
/// [`ProviderRefreshPolicy::next_attempt_at`] 的原有语义。
#[must_use]
pub(crate) fn initial_oauth_refresh_at(
    policy: ProviderRefreshPolicy,
    access_token_expires_at: Option<SystemTime>,
    observed_at: SystemTime,
    has_refresh_token: bool,
) -> Option<SystemTime> {
    if !has_refresh_token {
        return None;
    }
    let expires_at = access_token_expires_at?;
    let remaining = expires_at.duration_since(observed_at).ok()?;
    let initial_margin = policy
        .margin()
        .min(INITIAL_OAUTH_REFRESH_MAX_MARGIN)
        .min(remaining / 2);
    expires_at.checked_sub(initial_margin)
}

pub(crate) fn refresh_recovery_deadline(
    access_token_expires_at: Option<SystemTime>,
) -> Option<SystemTime> {
    access_token_expires_at.and_then(|expires_at| expires_at.checked_add(REFRESH_RECOVERY_WINDOW))
}

fn refresh_recovery_window_exhausted(
    access_token_expires_at: Option<SystemTime>,
    now: SystemTime,
) -> bool {
    refresh_recovery_deadline(access_token_expires_at).is_some_and(|deadline| deadline <= now)
}

fn bounded_refresh_retry_at(
    retry_at: SystemTime,
    access_token_expires_at: Option<SystemTime>,
    observed_at: SystemTime,
) -> SystemTime {
    let mut bounded = retry_at;
    if let Some(expires_at) = access_token_expires_at
        && expires_at > observed_at
        && bounded > expires_at
    {
        bounded = expires_at;
    }
    if let Some(deadline) = refresh_recovery_deadline(access_token_expires_at)
        && bounded > deadline
    {
        bounded = deadline;
    }
    bounded
}

fn oauth_refresh_retry_at(
    account_id: &ProviderAccountId,
    access_token_expires_at: Option<SystemTime>,
    observed_at: SystemTime,
    attempt: u32,
    reason: &'static str,
) -> Result<SystemTime, ProviderStoreError> {
    let retry_at = if attempt <= REFRESH_BACKOFF_MAX_ATTEMPTS {
        provider_refresh_backoff_at(account_id, observed_at, attempt, reason)?
    } else {
        provider_refresh_retry_at(account_id, observed_at, REFRESH_RECOVERY_DELAY, reason)?
    };
    Ok(bounded_refresh_retry_at(
        retry_at,
        access_token_expires_at,
        observed_at,
    ))
}

fn log_refresh_deferred(
    account_id: &ProviderAccountId,
    access_token_expires_at: Option<SystemTime>,
    attempt: u32,
    reason: &'static str,
    retry_at: SystemTime,
) {
    tracing::warn!(
        account_id = %account_id,
        attempt,
        reason,
        retry_at = %DateTime::<Utc>::from(retry_at),
        access_token_expires_at = ?access_token_expires_at.map(DateTime::<Utc>::from),
        recovery_deadline = ?refresh_recovery_deadline(access_token_expires_at)
            .map(DateTime::<Utc>::from),
        "OpenAI OAuth refresh deferred"
    );
}

fn refresh_due_at(account: &ProviderAccount, now: SystemTime) -> Option<SystemTime> {
    if refresh_recovery_window_exhausted(account.access_token_expires_at(), now) {
        // 即使旧 next_refresh_at 异常地落在 deadline 之后，也必须进入终态化路径。
        return refresh_recovery_deadline(account.access_token_expires_at());
    }
    account.next_refresh_at().filter(|next| *next <= now)
}

pub struct DueCodexCredential {
    pub account: ProviderAccount,
    pub secret: CodexOAuthSecret,
}

impl std::fmt::Debug for DueCodexCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DueCodexCredential")
            .field("account", &self.account)
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexCredentialRefreshOutcome {
    Refreshed {
        account_id: String,
        credential_revision: u64,
    },
    Invalidated {
        account_id: String,
    },
    Banned {
        account_id: String,
    },
    Transient {
        account_id: String,
    },
    LeaseUnavailable {
        account_id: String,
    },
    Stale {
        account_id: String,
    },
    Failed {
        account_id: String,
    },
}

#[derive(Debug, Error)]
pub enum CodexCredentialRefreshError {
    #[error(transparent)]
    Repository(#[from] CredentialRepositoryError),
    #[error(transparent)]
    Lease(#[from] ProviderStoreError),
    #[error("Codex refresh response is invalid")]
    InvalidRefreshResponse,
}

pub struct CodexCredentialRefreshService {
    repository: CodexCredentialRepository,
    refresher: Arc<dyn TokenRefresher>,
    leases: Arc<dyn ProviderLeasePort>,
    credential_state: Arc<dyn ProviderCredentialStatePort>,
    runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
}

impl CodexCredentialRefreshService {
    pub fn new(
        repository: CodexCredentialRepository,
        refresher: Arc<dyn TokenRefresher>,
        leases: Arc<dyn ProviderLeasePort>,
        credential_state: Arc<dyn ProviderCredentialStatePort>,
        runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
    ) -> Self {
        Self {
            repository,
            refresher,
            leases,
            credential_state,
            runtime_policy,
        }
    }

    pub async fn refresh_due(
        &self,
    ) -> Result<Vec<CodexCredentialRefreshOutcome>, CodexCredentialRefreshError> {
        self.refresh_due_excluding(&BTreeSet::new()).await
    }

    pub async fn refresh_due_excluding(
        &self,
        excluded: &BTreeSet<ProviderAccountId>,
    ) -> Result<Vec<CodexCredentialRefreshOutcome>, CodexCredentialRefreshError> {
        let policy = self.runtime_policy.load_refresh_policy().await?;
        let (due, mut outcomes) = self.list_due_refresh(excluded).await?;
        outcomes.reserve(due.len());
        let refreshed = stream::iter(due.into_iter().map(|credential| async move {
            let account_id = credential.account.id().to_string();
            (
                account_id,
                self.refresh_one_with_policy(credential, policy).await,
            )
        }))
        .buffer_unordered(policy.concurrency().get() as usize)
        .collect::<Vec<_>>()
        .await;
        for (account_id, result) in refreshed {
            match result {
                Ok(outcome) => outcomes.push(outcome),
                Err(error) => {
                    tracing::warn!(
                        account_id = %account_id,
                        error = %error,
                        "OpenAI OAuth refresh attempt failed"
                    );
                    outcomes.push(CodexCredentialRefreshOutcome::Failed { account_id });
                }
            }
        }
        Ok(outcomes)
    }

    async fn refresh_one_with_policy(
        &self,
        due: DueCodexCredential,
        policy: ProviderRefreshPolicy,
    ) -> Result<CodexCredentialRefreshOutcome, CodexCredentialRefreshError> {
        let account_id = due.account.id().to_string();
        if refresh_recovery_window_exhausted(
            due.account.access_token_expires_at(),
            SystemTime::now(),
        ) {
            return self
                .persist_terminal(
                    &due.account,
                    AccountAvailability::Expired,
                    "refresh_recovery_window_exhausted",
                    CodexCredentialRefreshOutcome::Invalidated { account_id },
                )
                .await;
        }
        let capacity = self
            .leases
            .try_acquire(ProviderLeaseRequest::RefreshCapacity(
                ProviderRefreshCapacityRequest::new(policy.concurrency()),
            ))
            .await?;
        let _capacity_guard = match capacity {
            ProviderLeaseAcquisition::Acquired(guard) => guard,
            ProviderLeaseAcquisition::Busy { .. } => {
                return Ok(CodexCredentialRefreshOutcome::LeaseUnavailable { account_id });
            }
        };
        let acquisition = self
            .leases
            .try_acquire(ProviderLeaseRequest::Refresh(
                ProviderRefreshLeaseRequest::new(due.account.id().clone(), due.account.revision()),
            ))
            .await?;
        let _guard = match acquisition {
            ProviderLeaseAcquisition::Acquired(guard) => guard,
            ProviderLeaseAcquisition::Busy { .. } => {
                return Ok(CodexCredentialRefreshOutcome::LeaseUnavailable { account_id });
            }
        };
        let refresh_token = due
            .secret
            .refresh_token
            .as_ref()
            .ok_or(CodexCredentialRefreshError::InvalidRefreshResponse)?;
        match self.refresher.refresh(refresh_token.expose_secret()).await {
            Ok(tokens) => self.persist_success(due, tokens, policy).await,
            Err(RefreshFailure::InvalidGrant) => {
                self.persist_terminal(
                    &due.account,
                    AccountAvailability::Expired,
                    "refresh_invalid_grant",
                    CodexCredentialRefreshOutcome::Invalidated { account_id },
                )
                .await
            }
            Err(RefreshFailure::Banned) => {
                self.persist_terminal(
                    &due.account,
                    AccountAvailability::Banned,
                    "account_banned",
                    CodexCredentialRefreshOutcome::Banned { account_id },
                )
                .await
            }
            Err(RefreshFailure::RetryableTransport) => {
                if self
                    .defer_refresh(&due.account, "transport-not-sent")
                    .await?
                {
                    Ok(CodexCredentialRefreshOutcome::Transient { account_id })
                } else {
                    Ok(CodexCredentialRefreshOutcome::Stale { account_id })
                }
            }
            Err(RefreshFailure::Transport) => {
                // 上游瞬态（429/5xx/超时/畸形响应等，发送状态未知）保留现有凭据、
                // 推进退避重试；仅 invalid_grant/banned 才终态失效账号。
                if self
                    .defer_refresh(&due.account, "transport-ambiguous")
                    .await?
                {
                    Ok(CodexCredentialRefreshOutcome::Transient { account_id })
                } else {
                    Ok(CodexCredentialRefreshOutcome::Stale { account_id })
                }
            }
        }
    }

    async fn list_due_refresh(
        &self,
        excluded: &BTreeSet<ProviderAccountId>,
    ) -> Result<
        (Vec<DueCodexCredential>, Vec<CodexCredentialRefreshOutcome>),
        CodexCredentialRefreshError,
    > {
        let now = SystemTime::now();
        let mut accounts = self
            .repository
            .store()
            .list_accounts()
            .await
            .map_err(CredentialRepositoryError::from)?;
        accounts.retain(|account| {
            account.provider().as_str() == PROVIDER_NAME
                && account.enabled()
                && account.has_refresh_token()
                && !excluded.contains(account.id())
                && !matches!(
                    account.availability(),
                    AccountAvailability::Expired
                        | AccountAvailability::Banned
                        | AccountAvailability::Invalid
                )
                && refresh_due_at(account, now).is_some()
        });
        accounts.sort_by_key(|account| (refresh_due_at(account, now), account.id().clone()));
        accounts.truncate(MAX_REFRESH_BATCH as usize);
        let mut due = Vec::with_capacity(accounts.len());
        let mut failures = Vec::new();
        for account in accounts {
            let account_id = account.id().to_string();
            match self.repository.load_runtime_credential(&account).await {
                Ok(runtime)
                    if runtime
                        .authentication
                        .oauth()
                        .is_some_and(|secret| secret.refresh_token.is_some()) =>
                {
                    let Some(secret) = runtime.authentication.oauth() else {
                        unreachable!("OAuth runtime authentication was checked above")
                    };
                    due.push(DueCodexCredential {
                        account,
                        secret: secret.clone(),
                    });
                }
                Ok(_) | Err(_) => {
                    failures.push(CodexCredentialRefreshOutcome::Failed { account_id });
                }
            }
        }
        Ok((due, failures))
    }

    async fn persist_success(
        &self,
        due: DueCodexCredential,
        tokens: TokenPair,
        policy: ProviderRefreshPolicy,
    ) -> Result<CodexCredentialRefreshOutcome, CodexCredentialRefreshError> {
        let TokenPair {
            access_token,
            refresh_token: rotated_refresh_token,
            expires_in,
        } = tokens;
        let refresh_token = rotated_refresh_token
            .map(SecretString::from)
            .or(due.secret.refresh_token);
        let secret = CodexOAuthSecret {
            access_token: SecretString::from(access_token),
            refresh_token,
            id_token: due.secret.id_token,
        };
        record_oauth_recovery(
            CodexOAuthRecoveryOperation::ScheduledRefresh,
            Some(due.account.id().as_str()),
            secret.access_token.expose_secret(),
            secret
                .refresh_token
                .as_ref()
                .map(ExposeSecret::expose_secret),
        );
        let observed_at = SystemTime::now();
        let access_token_expires_at = expires_in.and_then(|value| observed_at.checked_add(value));
        let next_refresh_at = match access_token_expires_at {
            Some(expires_at) => Some(DateTime::<Utc>::from(
                policy
                    .next_attempt_at(due.account.id(), expires_at, observed_at)
                    .map_err(CodexCredentialRefreshError::from)?,
            )),
            None => None,
        };
        let result = self
            .repository
            .rotate_refreshed_oauth_secret(
                &due.account,
                secret,
                access_token_expires_at,
                next_refresh_at.map(SystemTime::from),
            )
            .await;
        match result {
            Ok(revision) => {
                // 凭据完整成功轮换后清零连续失败计数，退避窗口重新从 base 起步。
                let _ = self
                    .credential_state
                    .clear_refresh_backoff(due.account.id())
                    .await;
                tracing::info!(
                    account_id = %due.account.id(),
                    credential_revision = revision.get(),
                    access_token_expires_at = ?access_token_expires_at.map(DateTime::<Utc>::from),
                    next_refresh_at = ?next_refresh_at,
                    "OpenAI OAuth refresh succeeded"
                );
                Ok(CodexCredentialRefreshOutcome::Refreshed {
                    account_id: due.account.id().to_string(),
                    credential_revision: revision.get(),
                })
            }
            Err(CredentialRepositoryError::RevisionConflict) => {
                Ok(CodexCredentialRefreshOutcome::Stale {
                    account_id: due.account.id().to_string(),
                })
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn persist_terminal(
        &self,
        account: &ProviderAccount,
        availability: AccountAvailability,
        reason: &'static str,
        outcome: CodexCredentialRefreshOutcome,
    ) -> Result<CodexCredentialRefreshOutcome, CodexCredentialRefreshError> {
        match self.repository.load_runtime_credential(account).await {
            Ok(_) => {}
            Err(CredentialRepositoryError::RevisionConflict) => {
                return Ok(CodexCredentialRefreshOutcome::Stale {
                    account_id: account.id().to_string(),
                });
            }
            Err(error) => return Err(error.into()),
        }
        match self
            .repository
            .apply_state_with_message(
                account,
                availability,
                SystemTime::now(),
                Some(reason.to_owned()),
            )
            .await
        {
            Ok(()) => {
                tracing::warn!(
                    account_id = %account.id(),
                    ?availability,
                    reason,
                    access_token_expires_at = ?account.access_token_expires_at()
                        .map(DateTime::<Utc>::from),
                    recovery_deadline = ?refresh_recovery_deadline(account.access_token_expires_at())
                        .map(DateTime::<Utc>::from),
                    "OpenAI OAuth refresh marked account terminal"
                );
                Ok(outcome)
            }
            Err(CredentialRepositoryError::RevisionConflict) => {
                Ok(CodexCredentialRefreshOutcome::Stale {
                    account_id: account.id().to_string(),
                })
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn defer_refresh(
        &self,
        account: &ProviderAccount,
        reason: &'static str,
    ) -> Result<bool, CodexCredentialRefreshError> {
        let attempt = self
            .credential_state
            .record_refresh_backoff(account.id(), REFRESH_BACKOFF_WINDOW)
            .await
            .unwrap_or(1);
        let retry_at = oauth_refresh_retry_at(
            account.id(),
            account.access_token_expires_at(),
            SystemTime::now(),
            attempt,
            reason,
        )?;
        match self.repository.defer_refresh(account, retry_at).await {
            Ok(_) => {
                log_refresh_deferred(
                    account.id(),
                    account.access_token_expires_at(),
                    attempt,
                    reason,
                    retry_at,
                );
                Ok(true)
            }
            Err(CredentialRepositoryError::RevisionConflict) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}
