//! Codex AT/RT 刷新状态机；Redis lease + ProviderAccountStore CAS，无 SQL。

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use futures::{StreamExt as _, stream};
use gateway_core::engine::credential::{
    AccountErrorReason, CredentialState, ProviderAccount, ProviderAccountId,
};
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
use super::token_client::{RefreshFailure, RefreshUpstreamFailure, TokenPair, TokenRefresher};
use super::types::{CodexOAuthSecret, parse_access_token_expiration};

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
    upstream_message: Option<&str>,
    upstream: Option<&RefreshUpstreamFailure>,
    retry_at: SystemTime,
) {
    tracing::warn!(
        account_id = %account_id,
        attempt,
        reason,
        upstream_message = ?upstream_message,
        upstream_status = ?upstream.map(RefreshUpstreamFailure::status),
        upstream_code = ?upstream.and_then(RefreshUpstreamFailure::code),
        upstream_type = ?upstream.and_then(RefreshUpstreamFailure::error_type),
        upstream_body = ?upstream.map(RefreshUpstreamFailure::body),
        retry_at = %DateTime::<Utc>::from(retry_at),
        access_token_expires_at = ?access_token_expires_at.map(DateTime::<Utc>::from),
        recovery_deadline = ?refresh_recovery_deadline(access_token_expires_at)
            .map(DateTime::<Utc>::from),
        "OpenAI OAuth refresh deferred"
    );
}

struct RefreshFailureContext<'a> {
    reason: &'static str,
    message: Option<String>,
    upstream: Option<&'a RefreshUpstreamFailure>,
}

impl<'a> RefreshFailureContext<'a> {
    const fn new(
        reason: &'static str,
        message: Option<String>,
        upstream: Option<&'a RefreshUpstreamFailure>,
    ) -> Self {
        Self {
            reason,
            message,
            upstream,
        }
    }
}

fn refresh_due_at(
    account: &ProviderAccount,
    policy: ProviderRefreshPolicy,
    now: SystemTime,
) -> Option<SystemTime> {
    if refresh_recovery_window_exhausted(account.access_token_expires_at(), now) {
        // 即使 retry-not-before 异常地落在 deadline 之后，也必须进入终态化路径。
        return refresh_recovery_deadline(account.access_token_expires_at());
    }
    let access_token_expires_at = account.access_token_expires_at()?;
    if !policy.is_refresh_due(access_token_expires_at, now) {
        return None;
    }
    if account
        .next_refresh_at()
        .is_some_and(|retry_at| retry_at > now)
    {
        return None;
    }
    Some(access_token_expires_at)
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
        let (due, mut outcomes) = self.list_due_refresh(excluded, policy).await?;
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
                    CredentialState::Expired,
                    AccountErrorReason::CredentialExpired,
                    RefreshFailureContext::new("refresh_recovery_window_exhausted", None, None),
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
            Ok(tokens) => self.persist_success(due, tokens).await,
            Err(RefreshFailure::InvalidGrant { message, upstream }) => {
                self.persist_terminal(
                    &due.account,
                    CredentialState::Expired,
                    AccountErrorReason::CredentialExpired,
                    RefreshFailureContext::new(
                        "refresh_invalid_grant",
                        message,
                        upstream.as_deref(),
                    ),
                    CodexCredentialRefreshOutcome::Invalidated { account_id },
                )
                .await
            }
            Err(RefreshFailure::Banned { message, upstream }) => {
                self.persist_terminal(
                    &due.account,
                    CredentialState::Banned,
                    AccountErrorReason::AccountBanned,
                    RefreshFailureContext::new("account_banned", message, upstream.as_deref()),
                    CodexCredentialRefreshOutcome::Banned { account_id },
                )
                .await
            }
            Err(RefreshFailure::RetryableTransport { message }) => {
                if self
                    .defer_refresh(&due.account, "transport-not-sent", Some(&message), None)
                    .await?
                {
                    Ok(CodexCredentialRefreshOutcome::Transient { account_id })
                } else {
                    Ok(CodexCredentialRefreshOutcome::Stale { account_id })
                }
            }
            Err(RefreshFailure::Transport { message, upstream }) => {
                // 上游瞬态（429/5xx/超时/畸形响应等，发送状态未知）保留现有凭据、
                // 推进退避重试；仅 invalid_grant/banned 才终态失效账号。
                if self
                    .defer_refresh(
                        &due.account,
                        "transport-ambiguous",
                        message.as_deref(),
                        upstream.as_deref(),
                    )
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
        policy: ProviderRefreshPolicy,
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
                && matches!(
                    account.credential_state(),
                    CredentialState::Unknown | CredentialState::Ready
                )
                && refresh_due_at(account, policy, now).is_some()
        });
        accounts
            .sort_by_key(|account| (refresh_due_at(account, policy, now), account.id().clone()));
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
    ) -> Result<CodexCredentialRefreshOutcome, CodexCredentialRefreshError> {
        let TokenPair {
            access_token,
            refresh_token: rotated_refresh_token,
            id_token: rotated_id_token,
        } = tokens;
        let access_token_expires_at = match access_token.as_deref() {
            Some(access_token) => parse_access_token_expiration(access_token).map(SystemTime::from),
            None => due.account.access_token_expires_at(),
        };
        let CodexOAuthSecret {
            access_token: current_access_token,
            refresh_token: current_refresh_token,
            id_token: current_id_token,
        } = due.secret;
        let refresh_token = rotated_refresh_token
            .map(SecretString::from)
            .or(current_refresh_token);
        let secret = CodexOAuthSecret {
            access_token: access_token
                .map(SecretString::from)
                .unwrap_or(current_access_token),
            refresh_token,
            id_token: rotated_id_token
                .map(SecretString::from)
                .or(current_id_token),
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
        // 成功轮换清掉仅用于失败退避的 retry-not-before；正常预刷新窗口由
        // worker 结合当前 runtime policy 动态判断，不写入账号时间字段。
        let next_refresh_at = None;
        let result = self
            .repository
            .rotate_refreshed_oauth_secret(
                &due.account,
                secret,
                access_token_expires_at,
                next_refresh_at,
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
                    refresh_retry_not_before = ?next_refresh_at.map(DateTime::<Utc>::from),
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
        credential_state: CredentialState,
        error_reason: AccountErrorReason,
        failure: RefreshFailureContext<'_>,
        outcome: CodexCredentialRefreshOutcome,
    ) -> Result<CodexCredentialRefreshOutcome, CodexCredentialRefreshError> {
        let RefreshFailureContext {
            reason,
            message,
            upstream,
        } = failure;
        match self.repository.load_runtime_credential(account).await {
            Ok(_) => {}
            Err(CredentialRepositoryError::RevisionConflict) => {
                return Ok(CodexCredentialRefreshOutcome::Stale {
                    account_id: account.id().to_string(),
                });
            }
            Err(error) => return Err(error.into()),
        }
        let persisted_message = message.as_deref().unwrap_or(reason).to_owned();
        match self
            .repository
            .apply_state_with_reason(
                account,
                credential_state,
                SystemTime::now(),
                Some(error_reason),
                Some(persisted_message),
            )
            .await
        {
            Ok(()) => {
                tracing::warn!(
                    account_id = %account.id(),
                    ?credential_state,
                    reason,
                    upstream_message = ?message.as_deref(),
                    upstream_status = ?upstream.map(RefreshUpstreamFailure::status),
                    upstream_code = ?upstream.and_then(RefreshUpstreamFailure::code),
                    upstream_type = ?upstream.and_then(RefreshUpstreamFailure::error_type),
                    upstream_body = ?upstream.map(RefreshUpstreamFailure::body),
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
        upstream_message: Option<&str>,
        upstream: Option<&RefreshUpstreamFailure>,
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
                    upstream_message,
                    upstream,
                    retry_at,
                );
                Ok(true)
            }
            Err(CredentialRepositoryError::RevisionConflict) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}
