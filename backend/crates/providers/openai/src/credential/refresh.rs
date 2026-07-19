//! Codex AT/RT 刷新状态机；Redis lease + ProviderAccountStore CAS，无 SQL。

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_core::engine::credential::{
    AccountAvailability, CredentialRevision, ProviderAccount, ProviderAccountId,
};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;

use super::repository::{CodexCredentialRepository, CredentialRepositoryError};
use super::token_client::{RefreshFailure, TokenPair, TokenRefresher};
use super::types::{CodexAccountProfile, CodexOAuthSecret, RotateCodexCredential};

const PROVIDER_NAME: &str = "openai";
const MAX_REFRESH_BATCH: u32 = 1_000;
const TRANSIENT_COOLDOWN: Duration = Duration::from_secs(30);

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

#[derive(Debug, Clone)]
pub struct CodexRefreshLeaseRequest {
    pub account_id: ProviderAccountId,
    pub credential_revision: CredentialRevision,
}

pub trait CodexRefreshLeaseGuard: Send + Sync + 'static {}
impl<T> CodexRefreshLeaseGuard for T where T: Send + Sync + 'static {}

pub enum CodexRefreshLeaseAcquisition {
    Acquired(Box<dyn CodexRefreshLeaseGuard>),
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CodexRefreshLeaseError {
    #[error("Codex refresh lease runtime is unavailable")]
    Unavailable,
}

#[async_trait]
pub trait CodexRefreshLeaseCoordinator: Send + Sync {
    async fn try_acquire(
        &self,
        request: &CodexRefreshLeaseRequest,
    ) -> Result<CodexRefreshLeaseAcquisition, CodexRefreshLeaseError>;
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
    Lease(#[from] CodexRefreshLeaseError),
    #[error("Codex refresh configuration is invalid")]
    InvalidConfiguration,
    #[error("Codex refresh response is invalid")]
    InvalidRefreshResponse,
}

pub struct CodexCredentialRefreshService {
    repository: CodexCredentialRepository,
    refresher: Arc<dyn TokenRefresher>,
    leases: Arc<dyn CodexRefreshLeaseCoordinator>,
    refresh_margin: Duration,
}

impl CodexCredentialRefreshService {
    pub fn new(
        repository: CodexCredentialRepository,
        refresher: Arc<dyn TokenRefresher>,
        leases: Arc<dyn CodexRefreshLeaseCoordinator>,
        refresh_margin: Duration,
    ) -> Result<Self, CodexCredentialRefreshError> {
        if refresh_margin.is_zero() {
            return Err(CodexCredentialRefreshError::InvalidConfiguration);
        }
        Ok(Self {
            repository,
            refresher,
            leases,
            refresh_margin,
        })
    }

    pub async fn refresh_due(
        &self,
        limit: u32,
    ) -> Result<Vec<CodexCredentialRefreshOutcome>, CodexCredentialRefreshError> {
        self.refresh_due_excluding(limit, &BTreeSet::new()).await
    }

    pub async fn refresh_due_excluding(
        &self,
        limit: u32,
        excluded: &BTreeSet<ProviderAccountId>,
    ) -> Result<Vec<CodexCredentialRefreshOutcome>, CodexCredentialRefreshError> {
        if limit == 0 || limit > MAX_REFRESH_BATCH {
            return Err(CodexCredentialRefreshError::InvalidConfiguration);
        }
        let (due, mut outcomes) = self.list_due_refresh(limit, excluded).await?;
        outcomes.reserve(due.len());
        for credential in due {
            let account_id = credential.account.id().to_string();
            match self.refresh_one(credential).await {
                Ok(outcome) => outcomes.push(outcome),
                Err(_) => outcomes.push(CodexCredentialRefreshOutcome::Failed { account_id }),
            }
        }
        Ok(outcomes)
    }

    pub async fn refresh_one(
        &self,
        due: DueCodexCredential,
    ) -> Result<CodexCredentialRefreshOutcome, CodexCredentialRefreshError> {
        let account_id = due.account.id().to_string();
        let acquisition = self
            .leases
            .try_acquire(&CodexRefreshLeaseRequest {
                account_id: due.account.id().clone(),
                credential_revision: due.account.revision(),
            })
            .await?;
        let _guard = match acquisition {
            CodexRefreshLeaseAcquisition::Acquired(guard) => guard,
            CodexRefreshLeaseAcquisition::Unavailable => {
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
            Err(RefreshFailure::InvalidGrant) => {
                self.invalidate(&due.account, AccountAvailability::Expired, "invalid_grant")
                    .await?;
                Ok(CodexCredentialRefreshOutcome::Invalidated { account_id })
            }
            Err(RefreshFailure::Banned) => {
                self.invalidate(&due.account, AccountAvailability::Banned, "account_banned")
                    .await?;
                Ok(CodexCredentialRefreshOutcome::Banned { account_id })
            }
            Err(RefreshFailure::RetryableTransport) => {
                self.cooldown(&due.account).await?;
                Ok(CodexCredentialRefreshOutcome::Transient { account_id })
            }
            Err(RefreshFailure::Transport) => {
                Ok(CodexCredentialRefreshOutcome::Ambiguous { account_id })
            }
        }
    }

    async fn list_due_refresh(
        &self,
        limit: u32,
        excluded: &BTreeSet<ProviderAccountId>,
    ) -> Result<
        (Vec<DueCodexCredential>, Vec<CodexCredentialRefreshOutcome>),
        CodexCredentialRefreshError,
    > {
        let now = SystemTime::now();
        let threshold = now.checked_add(self.refresh_margin).unwrap_or(now);
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
                && account
                    .next_refresh_at()
                    .is_some_and(|next| next <= threshold)
        });
        accounts.sort_by_key(|account| (account.next_refresh_at(), account.id().clone()));
        accounts.truncate(limit as usize);
        let mut due = Vec::with_capacity(accounts.len());
        let mut failures = Vec::new();
        for account in accounts {
            let account_id = account.id().to_string();
            match self.repository.load_runtime_credential(&account).await {
                Ok(runtime) if runtime.secret.refresh_token.is_some() => {
                    due.push(DueCodexCredential {
                        account,
                        secret: runtime.secret,
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
        if tokens.access_token.is_empty() || tokens.expires_in <= self.refresh_margin {
            return Err(CodexCredentialRefreshError::InvalidRefreshResponse);
        }
        let refresh_token = tokens
            .refresh_token
            .map(SecretString::from)
            .or(due.secret.refresh_token);
        let now = SystemTime::now();
        let expires_at = now
            .checked_add(tokens.expires_in)
            .ok_or(CodexCredentialRefreshError::InvalidRefreshResponse)?;
        let next_refresh_at = expires_at
            .checked_sub(self.refresh_margin)
            .ok_or(CodexCredentialRefreshError::InvalidRefreshResponse)?;
        let profile = CodexAccountProfile {
            email: due.account.email().map(str::to_owned),
            chatgpt_account_id: due
                .account
                .upstream_account_id()
                .unwrap_or(due.account.upstream_user_id())
                .to_owned(),
            chatgpt_user_id: Some(due.account.upstream_user_id().to_owned()),
            plan_type: due.account.plan_type().map(str::to_owned),
            access_token_expires_at: Some(DateTime::<Utc>::from(expires_at)),
            next_refresh_at: Some(DateTime::<Utc>::from(next_refresh_at)),
        };
        let result = self
            .repository
            .rotate_oauth_secret(RotateCodexCredential {
                account_id: due.account.id().to_string(),
                expected_credential_revision: due.account.revision().get(),
                secret: CodexOAuthSecret {
                    access_token: SecretString::from(tokens.access_token),
                    refresh_token,
                    id_token: due.secret.id_token,
                },
                verified_account: profile,
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

    async fn invalidate(
        &self,
        account: &ProviderAccount,
        availability: AccountAvailability,
        reason: &'static str,
    ) -> Result<(), CodexCredentialRefreshError> {
        self.repository
            .apply_state(
                account,
                availability,
                Some(reason.to_owned()),
                None,
                SystemTime::now(),
            )
            .await?;
        Ok(())
    }

    async fn cooldown(&self, account: &ProviderAccount) -> Result<(), CodexCredentialRefreshError> {
        let now = SystemTime::now();
        self.repository
            .apply_state(
                account,
                AccountAvailability::Cooldown,
                Some("refresh_transport".to_owned()),
                now.checked_add(TRANSIENT_COOLDOWN),
                now,
            )
            .await?;
        Ok(())
    }
}
