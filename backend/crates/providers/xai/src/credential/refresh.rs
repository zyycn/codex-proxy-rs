//! xAI OAuth refresh state machine；并发由 lease、写回由 credential revision CAS 保证。

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_core::engine::credential::{
    AccountAvailability, CredentialRevision, ProviderAccountId,
};
use gateway_core::routing::ProviderInstanceId;

use super::catalog::GrokCredentialCatalogService;
use super::repository::{
    GrokCredentialAdmin, GrokCredentialRepository, GrokCredentialRepositoryError,
};
use super::types::{
    GrokAccountProfile, GrokCredentialAvailability, GrokOAuthSecret,
    PreparedGrokCredentialRotation, RotateGrokCredential, RotateManagedGrokCredential,
    UpdateGrokCredentialState,
};
use crate::{
    DiscoveryDocument, FailureClass, GrokOAuthClient, OAuthError, RefreshTokenGrant,
    RefreshedTokenSet, SecretValue, TransportFailureKind,
};

const MAX_REFRESH_BATCH: u32 = 100;
const MAX_REFRESH_EXCLUSIONS: usize = 400;
const MAX_SECRET_BYTES: usize = 64 * 1_024;
const DISCOVERY_NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(5);
const TRANSIENT_COOLDOWN: Duration = Duration::from_secs(30);

/// 一个到期且已按 revision 读取明文 RT 的 xAI account。
pub struct DueGrokCredential {
    account_id: ProviderAccountId,
    provider_instance_id: ProviderInstanceId,
    credential_revision: CredentialRevision,
    refresh_token: SecretValue,
    id_token: Option<SecretValue>,
    scope: String,
    subject: String,
    email: Option<String>,
    upstream_account_id: Option<String>,
    plan_type: Option<String>,
    refresh_token_expires_at: Option<DateTime<Utc>>,
}

struct DueGrokRefreshBatch {
    credentials: Vec<DueGrokCredential>,
    failed_account_ids: Vec<ProviderAccountId>,
}

impl DueGrokCredential {
    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn credential_revision(&self) -> CredentialRevision {
        self.credential_revision
    }
}

impl fmt::Debug for DueGrokCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DueGrokCredential")
            .field("account_id", &self.account_id)
            .field("provider_instance_id", &self.provider_instance_id)
            .field("credential_revision", &self.credential_revision)
            .field("refresh_token", &"[REDACTED]")
            .field("id_token", &self.id_token.as_ref().map(|_| "[REDACTED]"))
            .field("scope", &"[REDACTED]")
            .field("subject", &"[REDACTED]")
            .field("email", &self.email.as_ref().map(|_| "[REDACTED]"))
            .field("refresh_token_expires_at", &self.refresh_token_expires_at)
            .finish()
    }
}

#[derive(Clone)]
pub struct GrokRefreshTokens {
    pub access_token: SecretValue,
    pub rotated_refresh_token: Option<SecretValue>,
    pub expires_in: Duration,
}

impl fmt::Debug for GrokRefreshTokens {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokRefreshTokens")
            .field("access_token", &"[REDACTED]")
            .field(
                "rotated_refresh_token",
                &self.rotated_refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum GrokRefreshFailure {
    #[error("xAI refresh token is invalid or expired")]
    InvalidGrant,
    #[error("xAI account is banned")]
    Banned,
    #[error("xAI refresh exchange has ambiguous send state")]
    Ambiguous,
    #[error("xAI refresh exchange failed before server processing")]
    Transient,
    #[error("xAI refresh response was rejected")]
    Rejected,
}

#[async_trait]
pub trait GrokCredentialRefresher: Send + Sync {
    async fn prepare_cycle(&self) -> Result<(), GrokRefreshFailure> {
        Ok(())
    }

    async fn refresh(
        &self,
        refresh_token: &SecretValue,
    ) -> Result<GrokRefreshTokens, GrokRefreshFailure>;
}

pub struct GrokOAuthRefreshClient {
    client: Arc<GrokOAuthClient>,
    discovery: tokio::sync::Mutex<CachedDiscovery>,
}

enum CachedDiscovery {
    Empty,
    Ready(Arc<DiscoveryDocument>),
    FailedUntil {
        retry_at: tokio::time::Instant,
        failure: GrokRefreshFailure,
    },
}

impl GrokOAuthRefreshClient {
    #[must_use]
    pub fn new(client: Arc<GrokOAuthClient>) -> Self {
        Self {
            client,
            discovery: tokio::sync::Mutex::new(CachedDiscovery::Empty),
        }
    }
}

#[async_trait]
impl GrokCredentialRefresher for GrokOAuthRefreshClient {
    async fn prepare_cycle(&self) -> Result<(), GrokRefreshFailure> {
        let mut cached = self.discovery.lock().await;
        match &*cached {
            CachedDiscovery::Ready(_) => return Ok(()),
            CachedDiscovery::FailedUntil { retry_at, failure }
                if *retry_at > tokio::time::Instant::now() =>
            {
                return Err(*failure);
            }
            CachedDiscovery::Empty | CachedDiscovery::FailedUntil { .. } => {}
        }
        match self.client.discover().await {
            Ok(discovery) => {
                *cached = CachedDiscovery::Ready(Arc::new(discovery));
                Ok(())
            }
            Err(error) => {
                let failure = classify_oauth_refresh_error(error);
                *cached = CachedDiscovery::FailedUntil {
                    retry_at: tokio::time::Instant::now() + DISCOVERY_NEGATIVE_CACHE_TTL,
                    failure,
                };
                Err(failure)
            }
        }
    }

    async fn refresh(
        &self,
        refresh_token: &SecretValue,
    ) -> Result<GrokRefreshTokens, GrokRefreshFailure> {
        let discovery = match &*self.discovery.lock().await {
            CachedDiscovery::Ready(discovery) => discovery.clone(),
            CachedDiscovery::Empty | CachedDiscovery::FailedUntil { .. } => {
                return Err(GrokRefreshFailure::Rejected);
            }
        };
        let refreshed = self
            .client
            .refresh(
                discovery.as_ref(),
                &RefreshTokenGrant::new(refresh_token.clone()),
            )
            .await
            .map_err(classify_oauth_refresh_error)?;
        refreshed_tokens(refreshed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrokRefreshLeaseRequest {
    pub provider_instance_id: ProviderInstanceId,
    pub account_id: ProviderAccountId,
    pub credential_revision: CredentialRevision,
}

pub trait GrokRefreshLeaseGuard: Send + Sync + 'static {}

impl<T> GrokRefreshLeaseGuard for T where T: Send + Sync + 'static {}

pub enum GrokRefreshLeaseAcquisition {
    Acquired(Box<dyn GrokRefreshLeaseGuard>),
    Unavailable,
}

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum GrokRefreshLeaseError {
    #[error("xAI credential refresh lease is unavailable")]
    Unavailable,
}

#[async_trait]
pub trait GrokRefreshLeaseCoordinator: Send + Sync {
    async fn try_acquire(
        &self,
        request: &GrokRefreshLeaseRequest,
    ) -> Result<GrokRefreshLeaseAcquisition, GrokRefreshLeaseError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrokCredentialRefreshOutcome {
    Refreshed {
        account_id: ProviderAccountId,
        credential_revision: CredentialRevision,
    },
    Invalidated {
        account_id: ProviderAccountId,
    },
    Ambiguous {
        account_id: ProviderAccountId,
    },
    Transient {
        account_id: ProviderAccountId,
    },
    Rejected {
        account_id: ProviderAccountId,
    },
    LeaseUnavailable {
        account_id: ProviderAccountId,
    },
    Stale {
        account_id: ProviderAccountId,
    },
    Failed {
        account_id: ProviderAccountId,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum GrokCredentialRefreshError {
    #[error(transparent)]
    Repository(#[from] GrokCredentialRepositoryError),
    #[error(transparent)]
    Lease(#[from] GrokRefreshLeaseError),
    #[error("xAI credential refresh configuration is invalid")]
    InvalidConfiguration,
    #[error("xAI OAuth refresh response is invalid")]
    InvalidRefreshResponse,
    #[error("xAI OAuth refresh preparation failed")]
    Preparation,
    #[error("xAI OAuth manual refresh was rejected: {0}")]
    ManualFailure(GrokRefreshFailure),
}

pub struct GrokCredentialRefreshService {
    repository: GrokCredentialRepository,
    refresher: Arc<dyn GrokCredentialRefresher>,
    catalog: Arc<GrokCredentialCatalogService>,
    leases: Arc<dyn GrokRefreshLeaseCoordinator>,
    refresh_margin: Duration,
}

impl GrokCredentialRefreshService {
    pub fn new(
        repository: GrokCredentialRepository,
        refresher: Arc<dyn GrokCredentialRefresher>,
        catalog: Arc<GrokCredentialCatalogService>,
        leases: Arc<dyn GrokRefreshLeaseCoordinator>,
        refresh_margin: Duration,
    ) -> Result<Self, GrokCredentialRefreshError> {
        if refresh_margin.is_zero() || chrono::Duration::from_std(refresh_margin).is_err() {
            return Err(GrokCredentialRefreshError::InvalidConfiguration);
        }
        Ok(Self {
            repository,
            refresher,
            catalog,
            leases,
            refresh_margin,
        })
    }

    pub async fn refresh_due(
        &self,
        limit: u32,
    ) -> Result<Vec<GrokCredentialRefreshOutcome>, GrokCredentialRefreshError> {
        if !self.prepare_cycle_if_due().await? {
            return Ok(Vec::new());
        }
        self.refresh_due_excluding(limit, &[]).await
    }

    pub async fn prepare_cycle_if_due(&self) -> Result<bool, GrokCredentialRefreshError> {
        if !self.repository.has_due_refresh(self.refresh_margin).await? {
            return Ok(false);
        }
        self.refresher
            .prepare_cycle()
            .await
            .map_err(|_| GrokCredentialRefreshError::Preparation)?;
        Ok(true)
    }

    /// 手工刷新一个指定 revision；只返回 Provider 验证后的 CAS command，不写 Store。
    pub async fn prepare_manual_refresh(
        &self,
        account_id: &ProviderAccountId,
        expected_revision: CredentialRevision,
    ) -> Result<PreparedGrokCredentialRotation, GrokCredentialRefreshError> {
        let current = self
            .repository
            .load_managed(account_id, expected_revision)
            .await?;
        let loaded = super::repository::loaded_from_core(current.clone())?;
        if loaded
            .refresh_token_expires_at
            .is_some_and(|expires_at| expires_at <= Utc::now())
        {
            return Err(GrokCredentialRefreshError::ManualFailure(
                GrokRefreshFailure::InvalidGrant,
            ));
        }
        self.refresher
            .prepare_cycle()
            .await
            .map_err(GrokCredentialRefreshError::ManualFailure)?;
        let lease = self
            .leases
            .try_acquire(&GrokRefreshLeaseRequest {
                provider_instance_id: current.account.instance().clone(),
                account_id: account_id.clone(),
                credential_revision: expected_revision,
            })
            .await?;
        let guard = match lease {
            GrokRefreshLeaseAcquisition::Acquired(guard) => guard,
            GrokRefreshLeaseAcquisition::Unavailable => {
                return Err(GrokRefreshLeaseError::Unavailable.into());
            }
        };
        let tokens = self
            .refresher
            .refresh(&loaded.refresh_token)
            .await
            .map_err(GrokCredentialRefreshError::ManualFailure)?;
        if tokens.access_token.is_empty() || tokens.access_token.len() > MAX_SECRET_BYTES {
            return Err(GrokCredentialRefreshError::InvalidRefreshResponse);
        }
        let rotated_refresh_token = tokens.rotated_refresh_token.is_some();
        let refresh_token = tokens
            .rotated_refresh_token
            .unwrap_or_else(|| loaded.refresh_token.clone());
        if refresh_token.is_empty() || refresh_token.len() > MAX_SECRET_BYTES {
            return Err(GrokCredentialRefreshError::InvalidRefreshResponse);
        }
        let (access_token_expires_at, next_refresh_at) =
            refreshed_deadlines(tokens.expires_in, self.refresh_margin)
                .ok_or(GrokCredentialRefreshError::InvalidRefreshResponse)?;
        let prepared = GrokCredentialAdmin
            .prepare_rotation(&RotateManagedGrokCredential {
                current,
                secret: GrokOAuthSecret {
                    access_token: tokens.access_token,
                    refresh_token,
                    id_token: loaded.id_token,
                    scope: loaded.scope,
                },
                verified_account: GrokAccountProfile {
                    subject: loaded.account.upstream_user_id().to_owned(),
                    email: loaded.account.email().map(str::to_owned),
                    upstream_account_id: loaded.account.upstream_account_id().map(str::to_owned),
                    plan_type: loaded.account.plan_type().map(str::to_owned),
                    access_token_expires_at,
                    refresh_token_expires_at: if rotated_refresh_token {
                        None
                    } else {
                        loaded.refresh_token_expires_at
                    },
                    next_refresh_at: Some(next_refresh_at),
                },
            })
            .map_err(GrokCredentialRefreshError::from)?;
        Ok(prepared.with_refresh_guard(guard))
    }

    pub async fn refresh_due_excluding(
        &self,
        limit: u32,
        excluded_account_ids: &[ProviderAccountId],
    ) -> Result<Vec<GrokCredentialRefreshOutcome>, GrokCredentialRefreshError> {
        let batch = self
            .repository
            .list_due_refresh_batch_excluding(self.refresh_margin, limit, excluded_account_ids)
            .await?;
        let mut outcomes = batch
            .failed_account_ids
            .into_iter()
            .map(|account_id| GrokCredentialRefreshOutcome::Failed { account_id })
            .collect::<Vec<_>>();
        outcomes.reserve(batch.credentials.len());
        for credential in batch.credentials {
            let account_id = credential.account_id.clone();
            match self.refresh_one(credential).await {
                Ok(outcome) => outcomes.push(outcome),
                Err(_) => outcomes.push(GrokCredentialRefreshOutcome::Failed { account_id }),
            }
        }
        Ok(outcomes)
    }

    pub async fn refresh_one(
        &self,
        credential: DueGrokCredential,
    ) -> Result<GrokCredentialRefreshOutcome, GrokCredentialRefreshError> {
        let account_id = credential.account_id.clone();
        if credential
            .refresh_token_expires_at
            .is_some_and(|expires_at| expires_at <= Utc::now())
        {
            return self
                .persist_terminal_failure(
                    credential,
                    GrokCredentialAvailability::Expired,
                    "refresh_token_expired",
                )
                .await;
        }
        let lease = self
            .leases
            .try_acquire(&GrokRefreshLeaseRequest {
                provider_instance_id: credential.provider_instance_id.clone(),
                account_id: account_id.clone(),
                credential_revision: credential.credential_revision,
            })
            .await?;
        let _guard = match lease {
            GrokRefreshLeaseAcquisition::Acquired(guard) => guard,
            GrokRefreshLeaseAcquisition::Unavailable => {
                return Ok(GrokCredentialRefreshOutcome::LeaseUnavailable { account_id });
            }
        };

        match self.refresher.refresh(&credential.refresh_token).await {
            Ok(tokens) => self.persist_success(credential, tokens).await,
            Err(GrokRefreshFailure::InvalidGrant) => {
                self.persist_terminal_failure(
                    credential,
                    GrokCredentialAvailability::Expired,
                    "refresh_invalid_grant",
                )
                .await
            }
            Err(GrokRefreshFailure::Banned) => {
                self.persist_terminal_failure(
                    credential,
                    GrokCredentialAvailability::Banned,
                    "account_banned",
                )
                .await
            }
            Err(GrokRefreshFailure::Ambiguous) => self
                .persist_terminal_failure(
                    credential,
                    GrokCredentialAvailability::Invalid,
                    "refresh_ambiguous",
                )
                .await
                .map(|_| GrokCredentialRefreshOutcome::Ambiguous { account_id }),
            Err(GrokRefreshFailure::Transient) => {
                self.persist_cooldown(&credential, "refresh_transient")
                    .await?;
                Ok(GrokCredentialRefreshOutcome::Transient { account_id })
            }
            Err(GrokRefreshFailure::Rejected) => {
                self.persist_cooldown(&credential, "refresh_rejected")
                    .await?;
                Ok(GrokCredentialRefreshOutcome::Rejected { account_id })
            }
        }
    }

    async fn persist_success(
        &self,
        credential: DueGrokCredential,
        tokens: GrokRefreshTokens,
    ) -> Result<GrokCredentialRefreshOutcome, GrokCredentialRefreshError> {
        if tokens.access_token.is_empty() || tokens.access_token.len() > MAX_SECRET_BYTES {
            return Err(GrokCredentialRefreshError::InvalidRefreshResponse);
        }
        let rotated_refresh_token = tokens.rotated_refresh_token.is_some();
        let refresh_token = tokens
            .rotated_refresh_token
            .unwrap_or_else(|| credential.refresh_token.clone());
        if refresh_token.is_empty() || refresh_token.len() > MAX_SECRET_BYTES {
            return Err(GrokCredentialRefreshError::InvalidRefreshResponse);
        }
        let (access_expires_at, next_refresh_at) =
            refreshed_deadlines(tokens.expires_in, self.refresh_margin)
                .ok_or(GrokCredentialRefreshError::InvalidRefreshResponse)?;
        let account_id = credential.account_id.clone();
        let access_token = tokens.access_token.clone();
        let subject = credential.subject.clone();
        let email = credential.email.clone();
        let record = match self
            .repository
            .rotate_oauth_credential(&RotateGrokCredential {
                account_id: account_id.clone(),
                expected_revision: credential.credential_revision,
                secret: GrokOAuthSecret {
                    access_token: tokens.access_token,
                    refresh_token,
                    id_token: credential.id_token,
                    scope: credential.scope,
                },
                verified_account: GrokAccountProfile {
                    subject: credential.subject,
                    email: credential.email,
                    upstream_account_id: credential.upstream_account_id,
                    plan_type: credential.plan_type,
                    access_token_expires_at: access_expires_at,
                    refresh_token_expires_at: if rotated_refresh_token {
                        None
                    } else {
                        credential.refresh_token_expires_at
                    },
                    next_refresh_at: Some(next_refresh_at),
                },
            })
            .await
        {
            Ok(record) => record,
            Err(error) if stale_repository_error(&error) => {
                return Ok(GrokCredentialRefreshOutcome::Stale { account_id });
            }
            Err(error) => return Err(error.into()),
        };

        if let Ok(seed) = self
            .catalog
            .fetch_seed(
                access_token,
                SecretValue::new(subject),
                email.map(SecretValue::new),
                crate::transport::GROK_CLIENT_VERSION,
            )
            .await
        {
            let _ = self
                .catalog
                .cache_seed(account_id.clone(), record.credential_revision, seed)
                .await;
        }
        Ok(GrokCredentialRefreshOutcome::Refreshed {
            account_id,
            credential_revision: record.credential_revision,
        })
    }

    async fn persist_terminal_failure(
        &self,
        credential: DueGrokCredential,
        availability: GrokCredentialAvailability,
        reason: &str,
    ) -> Result<GrokCredentialRefreshOutcome, GrokCredentialRefreshError> {
        let account_id = credential.account_id.clone();
        match self
            .repository
            .update_state(&UpdateGrokCredentialState {
                account_id: account_id.clone(),
                expected_revision: credential.credential_revision,
                availability,
                availability_reason: Some(reason.to_owned()),
                cooldown_until: None,
                observed_at: Utc::now(),
            })
            .await
        {
            Ok(()) => Ok(GrokCredentialRefreshOutcome::Invalidated { account_id }),
            Err(error) if stale_repository_error(&error) => {
                Ok(GrokCredentialRefreshOutcome::Stale { account_id })
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn persist_cooldown(
        &self,
        credential: &DueGrokCredential,
        reason: &str,
    ) -> Result<(), GrokCredentialRefreshError> {
        let cooldown_until = chrono::Duration::from_std(TRANSIENT_COOLDOWN)
            .ok()
            .and_then(|duration| Utc::now().checked_add_signed(duration));
        self.repository
            .update_state(&UpdateGrokCredentialState {
                account_id: credential.account_id.clone(),
                expected_revision: credential.credential_revision,
                availability: GrokCredentialAvailability::Cooldown,
                availability_reason: Some(reason.to_owned()),
                cooldown_until,
                observed_at: Utc::now(),
            })
            .await
            .map_err(Into::into)
    }
}

impl GrokCredentialRepository {
    pub async fn has_due_refresh(
        &self,
        refresh_before: Duration,
    ) -> Result<bool, GrokCredentialRepositoryError> {
        Ok(self
            .list_all_accounts()
            .await?
            .iter()
            .any(|account| account_due(account, refresh_before, SystemTime::now())))
    }

    pub async fn list_due_refresh(
        &self,
        refresh_before: Duration,
        limit: u32,
    ) -> Result<Vec<DueGrokCredential>, GrokCredentialRepositoryError> {
        self.list_due_refresh_batch_excluding(refresh_before, limit, &[])
            .await
            .map(|batch| batch.credentials)
    }

    async fn list_due_refresh_batch_excluding(
        &self,
        refresh_before: Duration,
        limit: u32,
        excluded_account_ids: &[ProviderAccountId],
    ) -> Result<DueGrokRefreshBatch, GrokCredentialRepositoryError> {
        if limit == 0 || limit > MAX_REFRESH_BATCH {
            return Err(GrokCredentialRepositoryError::InvalidInput("refresh_limit"));
        }
        if excluded_account_ids.len() > MAX_REFRESH_EXCLUSIONS {
            return Err(GrokCredentialRepositoryError::InvalidInput(
                "refresh_exclusions",
            ));
        }
        let excluded = excluded_account_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let now = SystemTime::now();
        let mut accounts = self
            .list_all_accounts()
            .await?
            .into_iter()
            .filter(|account| {
                !excluded.contains(account.id()) && account_due(account, refresh_before, now)
            })
            .collect::<Vec<_>>();
        accounts.sort_by_key(|account| {
            (
                account.next_refresh_at(),
                account.access_token_expires_at(),
                account.id().clone(),
            )
        });
        accounts.truncate(limit as usize);

        let mut due = Vec::with_capacity(accounts.len());
        let mut failed_account_ids = Vec::new();
        for account in accounts {
            let loaded = match self.load(account.id(), account.revision()).await {
                Ok(loaded) => loaded,
                Err(_) => {
                    failed_account_ids.push(account.id().clone());
                    continue;
                }
            };
            due.push(DueGrokCredential {
                account_id: account.id().clone(),
                provider_instance_id: account.instance().clone(),
                credential_revision: account.revision(),
                refresh_token: loaded.refresh_token,
                id_token: loaded.id_token,
                scope: loaded.scope,
                subject: account.upstream_user_id().to_owned(),
                email: account.email().map(str::to_owned),
                upstream_account_id: account.upstream_account_id().map(str::to_owned),
                plan_type: account.plan_type().map(str::to_owned),
                refresh_token_expires_at: loaded.refresh_token_expires_at,
            });
        }
        Ok(DueGrokRefreshBatch {
            credentials: due,
            failed_account_ids,
        })
    }
}

fn account_due(
    account: &gateway_core::engine::credential::ProviderAccount,
    refresh_before: Duration,
    now: SystemTime,
) -> bool {
    let availability = account.availability();
    account.enabled()
        && account.has_refresh_token()
        && !matches!(
            availability,
            AccountAvailability::Expired
                | AccountAvailability::Banned
                | AccountAvailability::Invalid
        )
        && (availability != AccountAvailability::Cooldown
            || account.cooldown_until().is_some_and(|until| until <= now))
        && (account.next_refresh_at().is_some_and(|next| next <= now)
            || account
                .access_token_expires_at()
                .duration_since(now)
                .is_ok_and(|remaining| remaining <= refresh_before))
}

fn refreshed_tokens(tokens: RefreshedTokenSet) -> Result<GrokRefreshTokens, GrokRefreshFailure> {
    let expires_in = tokens.expires_in().ok_or(GrokRefreshFailure::Rejected)?;
    Ok(GrokRefreshTokens {
        access_token: tokens.access_token().clone(),
        rotated_refresh_token: tokens.rotated_refresh_token().cloned(),
        expires_in,
    })
}

fn classify_oauth_refresh_error(error: OAuthError) -> GrokRefreshFailure {
    if let OAuthError::Transport { kind, .. } = &error {
        return match kind {
            TransportFailureKind::NotSent | TransportFailureKind::Tls => {
                GrokRefreshFailure::Transient
            }
            TransportFailureKind::Ambiguous | TransportFailureKind::Timeout => {
                GrokRefreshFailure::Ambiguous
            }
        };
    }
    match error.class() {
        FailureClass::CredentialPermanent => GrokRefreshFailure::InvalidGrant,
        FailureClass::Ambiguous => GrokRefreshFailure::Ambiguous,
        FailureClass::Transient
        | FailureClass::ConfigurationPermanent
        | FailureClass::UserActionRequired
        | FailureClass::Security
        | FailureClass::Unsupported => GrokRefreshFailure::Rejected,
    }
}

fn refreshed_deadlines(
    expires_in: Duration,
    refresh_margin: Duration,
) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    if expires_in.is_zero() {
        return None;
    }
    let lead = refresh_margin.min(expires_in / 2);
    if lead.is_zero() || lead >= expires_in {
        return None;
    }
    let expires = chrono::Duration::from_std(expires_in).ok()?;
    let lead = chrono::Duration::from_std(lead).ok()?;
    let access_expires_at = Utc::now().checked_add_signed(expires)?;
    let next_refresh_at = access_expires_at.checked_sub_signed(lead)?;
    Some((access_expires_at, next_refresh_at))
}

fn stale_repository_error(error: &GrokCredentialRepositoryError) -> bool {
    matches!(
        error,
        GrokCredentialRepositoryError::CredentialNotFound
            | GrokCredentialRepositoryError::Conflict
            | GrokCredentialRepositoryError::StaleCredentialRevision
    )
}
