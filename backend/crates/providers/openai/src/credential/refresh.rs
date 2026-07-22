//! Codex AT/RT 刷新状态机；Redis lease + ProviderAccountStore CAS，无 SQL。

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use futures::{StreamExt as _, stream};
use gateway_core::engine::credential::{AccountAvailability, ProviderAccount, ProviderAccountId};
use gateway_core::provider_ports::{
    ProviderLeaseAcquisition, ProviderLeasePort, ProviderLeaseRequest,
    ProviderRefreshCapacityRequest, ProviderRefreshLeaseRequest, ProviderRefreshPolicy,
    ProviderRuntimePolicyPort, ProviderStoreError, provider_refresh_retry_at,
};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;

use super::identity::{
    CodexAccountIdentityVerifier, CodexIdentityExpectation, CodexIdentityVerification,
    CodexIdentityVerificationError,
};
use super::repository::{CodexCredentialRepository, CredentialRepositoryError};
use super::token_client::{RefreshFailure, TokenPair, TokenRefresher};
use super::types::{CodexCredentialPrincipal, CodexOAuthSecret, RotateCodexCredential};

const PROVIDER_NAME: &str = "openai";
const MAX_REFRESH_BATCH: u32 = 1_000;
const REFRESH_RETRY_DELAY: Duration = Duration::from_secs(10 * 60);

pub struct DueCodexCredential {
    pub account: ProviderAccount,
    pub secret: CodexOAuthSecret,
    principal: CodexCredentialPrincipal,
    installation_id: String,
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
    Ambiguous {
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
    identity: Arc<dyn CodexAccountIdentityVerifier>,
    leases: Arc<dyn ProviderLeasePort>,
    runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
}

impl CodexCredentialRefreshService {
    pub fn new(
        repository: CodexCredentialRepository,
        refresher: Arc<dyn TokenRefresher>,
        identity: Arc<dyn CodexAccountIdentityVerifier>,
        leases: Arc<dyn ProviderLeasePort>,
        runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
    ) -> Self {
        Self {
            repository,
            refresher,
            identity,
            leases,
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
                Err(_) => outcomes.push(CodexCredentialRefreshOutcome::Failed { account_id }),
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
                    "invalid_grant",
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
                self.persist_terminal(
                    &due.account,
                    AccountAvailability::Invalid,
                    "refresh_ambiguous",
                    CodexCredentialRefreshOutcome::Ambiguous { account_id },
                )
                .await
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
                && (account.availability() != AccountAvailability::Cooldown
                    || account.cooldown_until().is_some_and(|until| until <= now))
                && account.next_refresh_at().is_some_and(|next| next <= now)
        });
        accounts.sort_by_key(|account| (account.next_refresh_at(), account.id().clone()));
        accounts.truncate(MAX_REFRESH_BATCH as usize);
        let mut due = Vec::with_capacity(accounts.len());
        let mut failures = Vec::new();
        for account in accounts {
            let account_id = account.id().to_string();
            match self.repository.load_runtime_credential(&account).await {
                Ok(runtime) if runtime.secret.refresh_token.is_some() => {
                    due.push(DueCodexCredential {
                        account,
                        secret: runtime.secret,
                        principal: runtime.principal,
                        installation_id: runtime.installation_id,
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
        if tokens.access_token.is_empty() || tokens.expires_in.is_zero() {
            return Err(CodexCredentialRefreshError::InvalidRefreshResponse);
        }
        let refresh_token = tokens
            .refresh_token
            .map(SecretString::from)
            .or(due.secret.refresh_token);
        let account_id = due
            .account
            .upstream_account_id()
            .ok_or(CodexCredentialRefreshError::InvalidRefreshResponse)?;
        let expectation = CodexIdentityExpectation::current(
            due.principal.oauth_subject,
            due.principal.poid,
            account_id.to_owned(),
            due.account.upstream_user_id().to_owned(),
            due.installation_id,
        )
        .map_err(|_| CodexCredentialRefreshError::InvalidRefreshResponse)?;
        let secret = CodexOAuthSecret {
            access_token: SecretString::from(tokens.access_token),
            refresh_token,
            id_token: due.secret.id_token,
        };
        let verification = match self.identity.verify(&secret, &expectation).await {
            Ok(verification) => verification,
            Err(CodexIdentityVerificationError::Rejected) => {
                return self
                    .persist_terminal(
                        &due.account,
                        AccountAvailability::Invalid,
                        "refreshed_identity_rejected",
                        CodexCredentialRefreshOutcome::Invalidated {
                            account_id: due.account.id().to_string(),
                        },
                    )
                    .await;
            }
            Err(CodexIdentityVerificationError::Unavailable) => {
                return self
                    .persist_terminal(
                        &due.account,
                        AccountAvailability::Invalid,
                        "refreshed_identity_unavailable",
                        CodexCredentialRefreshOutcome::Ambiguous {
                            account_id: due.account.id().to_string(),
                        },
                    )
                    .await;
            }
        };
        let profile = match verification {
            CodexIdentityVerification::Complete(profile) => profile,
            CodexIdentityVerification::SignedOnly(signed) => {
                let retry_at = provider_refresh_retry_at(
                    due.account.id(),
                    SystemTime::now(),
                    REFRESH_RETRY_DELAY,
                    "identity-completion",
                )?;
                match self
                    .repository
                    .rotate_signed_secret(&due.account, secret, &signed, retry_at)
                    .await
                {
                    Ok(_) => {
                        return Ok(CodexCredentialRefreshOutcome::Transient {
                            account_id: due.account.id().to_string(),
                        });
                    }
                    Err(CredentialRepositoryError::RevisionConflict) => {
                        return Ok(CodexCredentialRefreshOutcome::Stale {
                            account_id: due.account.id().to_string(),
                        });
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        };
        let next_refresh_at = policy
            .next_attempt_at(
                due.account.id(),
                profile
                    .access_token_expires_at
                    .map(SystemTime::from)
                    .ok_or(CodexCredentialRefreshError::InvalidRefreshResponse)?,
                SystemTime::now(),
            )
            .map(DateTime::<Utc>::from)
            .map_err(CodexCredentialRefreshError::from)?;
        let result = self
            .repository
            .rotate_oauth_secret(RotateCodexCredential {
                account_id: due.account.id().to_string(),
                expected_credential_revision: due.account.revision().get(),
                secret,
                verified_account: profile,
                next_refresh_at: Some(next_refresh_at),
            })
            .await;
        match result {
            Ok(revision) => Ok(CodexCredentialRefreshOutcome::Refreshed {
                account_id: due.account.id().to_string(),
                credential_revision: revision.get(),
            }),
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
            .apply_state(
                account,
                availability,
                Some(reason.to_owned()),
                None,
                SystemTime::now(),
            )
            .await
        {
            Ok(()) => Ok(outcome),
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
        let retry_at = provider_refresh_retry_at(
            account.id(),
            SystemTime::now(),
            REFRESH_RETRY_DELAY,
            reason,
        )?;
        match self.repository.defer_refresh(account, retry_at).await {
            Ok(_) => Ok(true),
            Err(CredentialRepositoryError::RevisionConflict) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}
