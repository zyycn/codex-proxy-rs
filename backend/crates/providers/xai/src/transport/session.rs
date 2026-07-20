use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, SystemTime};

use gateway_core::engine::credential::{
    AccountSelectionPolicy, CredentialRevision, ProviderAccountId,
};
use gateway_core::engine::provider::ProviderResource;
use gateway_core::routing::{ProviderInstanceId, UpstreamModelId};

use crate::SecretValue;

/// Opaque, pseudonymous egress/session key understood by the injected
/// inference transport.
#[derive(Clone, PartialEq, Eq)]
pub struct GrokSessionBinding(String);

impl GrokSessionBinding {
    /// Creates a bounded, non-secret binding reference.
    ///
    /// # Errors
    ///
    /// Rejects empty, oversized, control-character, or reserved values.
    pub fn new(value: impl Into<String>) -> Result<Self, GrokSessionDataError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || value.starts_with("__")
            || value.chars().any(char::is_control)
        {
            return Err(GrokSessionDataError::InvalidBinding);
        }
        Ok(Self(value))
    }

    /// Returns the pseudonymous transport lookup key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for GrokSessionBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GrokSessionBinding([PSEUDONYM])")
    }
}

/// Selector-owned guard for credential, capacity, and egress affinity.
pub trait GrokSessionLeaseGuard: Send + Sync + 'static {}

impl<T> GrokSessionLeaseGuard for T where T: Send + Sync + 'static {}

/// One selected OAuth session and its owned live lease.
pub struct SelectedGrokSession {
    account_id: ProviderAccountId,
    credential_revision: CredentialRevision,
    access_token: SecretValue,
    user_id: SecretValue,
    email: Option<SecretValue>,
    binding: GrokSessionBinding,
    _guard: Box<dyn GrokSessionLeaseGuard>,
}

impl SelectedGrokSession {
    /// Constructs a selected session from Provider-owned plaintext OAuth material.
    ///
    /// # Errors
    ///
    /// Rejects a credential-less snapshot or malformed auth/header values.
    pub fn new(
        account_id: ProviderAccountId,
        credential_revision: CredentialRevision,
        access_token: SecretValue,
        user_id: SecretValue,
        email: Option<SecretValue>,
        binding: GrokSessionBinding,
        guard: impl GrokSessionLeaseGuard,
    ) -> Result<Self, GrokSessionDataError> {
        if !valid_secret_header(&access_token, 64 * 1024)
            || !valid_secret_header(&user_id, 1_024)
            || email
                .as_ref()
                .is_some_and(|value| !valid_secret_header(value, 1_024))
        {
            return Err(GrokSessionDataError::InvalidSecretValue);
        }
        Ok(Self {
            account_id,
            credential_revision,
            access_token,
            user_id,
            email,
            binding,
            _guard: Box::new(guard),
        })
    }

    /// Returns the selected account ID.
    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    /// Returns the selector-frozen credential revision.
    #[must_use]
    pub const fn credential_revision(&self) -> CredentialRevision {
        self.credential_revision
    }

    /// Returns metadata recorded by Core for this upstream call.
    #[must_use]
    pub fn resource(&self) -> ProviderResource {
        ProviderResource::Account {
            id: self.account_id.clone(),
            revision: self.credential_revision,
        }
    }

    /// Returns the OAuth access token for explicit header construction.
    #[must_use]
    pub const fn access_token(&self) -> &SecretValue {
        &self.access_token
    }

    /// Returns the verified user ID for official proxy headers.
    #[must_use]
    pub const fn user_id(&self) -> &SecretValue {
        &self.user_id
    }

    /// Returns the optional verified email for official proxy headers.
    #[must_use]
    pub const fn email(&self) -> Option<&SecretValue> {
        self.email.as_ref()
    }

    /// Returns the pseudonymous egress/session transport binding.
    #[must_use]
    pub const fn binding(&self) -> &GrokSessionBinding {
        &self.binding
    }
}

impl fmt::Debug for SelectedGrokSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SelectedGrokSession")
            .field("account_id", &self.account_id)
            .field("credential_revision", &self.credential_revision)
            .field("access_token", &"[REDACTED]")
            .field("user_id", &"[REDACTED]")
            .field("email", &self.email.as_ref().map(|_| "[REDACTED]"))
            .field("binding", &self.binding)
            .field("guard", &"[LEASE]")
            .finish()
    }
}

/// Input to one selector call. It owns a frozen, secret-free attempt view.
#[derive(Debug, Clone)]
pub struct GrokSessionSelection {
    provider_instance_id: ProviderInstanceId,
    upstream_model: UpstreamModelId,
    excluded_accounts: BTreeSet<ProviderAccountId>,
    required_account: Option<ProviderAccountId>,
    account_selection_policy: AccountSelectionPolicy,
    deadline: SystemTime,
}

impl GrokSessionSelection {
    /// Creates the immutable selection request.
    #[must_use]
    pub fn new(
        provider_instance_id: ProviderInstanceId,
        upstream_model: UpstreamModelId,
        excluded_accounts: BTreeSet<ProviderAccountId>,
        required_account: Option<ProviderAccountId>,
        account_selection_policy: AccountSelectionPolicy,
        deadline: SystemTime,
    ) -> Self {
        Self {
            provider_instance_id,
            upstream_model,
            excluded_accounts,
            required_account,
            account_selection_policy,
            deadline,
        }
    }

    /// Returns the frozen provider instance ID.
    #[must_use]
    pub const fn provider_instance_id(&self) -> &ProviderInstanceId {
        &self.provider_instance_id
    }

    /// Returns the frozen upstream model.
    #[must_use]
    pub const fn upstream_model(&self) -> &UpstreamModelId {
        &self.upstream_model
    }

    /// Returns accounts already attempted by the coordinator.
    #[must_use]
    pub const fn excluded_accounts(&self) -> &BTreeSet<ProviderAccountId> {
        &self.excluded_accounts
    }

    /// Returns the sole account permitted for this call, when constrained by Core.
    #[must_use]
    pub const fn required_account(&self) -> Option<&ProviderAccountId> {
        self.required_account.as_ref()
    }

    /// Returns the frozen global account scheduling policy.
    #[must_use]
    pub const fn account_selection_policy(&self) -> AccountSelectionPolicy {
        self.account_selection_policy
    }

    /// Returns the absolute deadline that bounds the scheduling lease.
    #[must_use]
    pub const fn deadline(&self) -> SystemTime {
        self.deadline
    }
}

/// Future returned by a Grok session selector.
pub type GrokSessionSelectorFuture<'a> = Pin<
    Box<dyn Future<Output = Result<SelectedGrokSession, GrokSessionSelectorError>> + Send + 'a>,
>;

/// Credential-scoped upstream failure safe to persist as availability feedback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrokCredentialFailure {
    /// The selected OAuth token was rejected.
    Unauthorized,
    /// The selected OAuth account was rate limited.
    RateLimited {
        /// Already parsed and redacted delay; no raw upstream header is retained.
        retry_after: Option<Duration>,
    },
    /// The selected OAuth account exhausted its quota.
    QuotaExhausted,
}

/// Future returned after one best-effort credential feedback write.
pub type GrokCredentialFeedbackFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Runtime port that selects exactly one eligible OAuth session and acquires
/// all credential/capacity/egress leases required for the stream lifetime.
pub trait GrokSessionSelector: Send + Sync {
    /// Performs one selection without internal provider fallback.
    fn select(&self, request: GrokSessionSelection) -> GrokSessionSelectorFuture<'_>;

    /// Persists one classified failure without retrying or replacing the error.
    fn record_failure<'a>(
        &'a self,
        session: &'a SelectedGrokSession,
        failure: GrokCredentialFailure,
    ) -> GrokCredentialFeedbackFuture<'a>;
}

/// Secret-free selector failure.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GrokSessionSelectorError {
    /// No session satisfies model, state, and exclusion constraints.
    #[error("no eligible Grok Build session is available")]
    NoEligibleSession,
    /// All eligible sessions are currently leased or rate-spaced.
    #[error("Grok Build session capacity is unavailable")]
    CapacityUnavailable {
        /// Optional selector-derived delay.
        retry_after: Option<Duration>,
    },
    /// Session metadata or Provider-owned plaintext secret is invalid.
    #[error("Grok Build session data is invalid")]
    InvalidSession,
    /// Selector backing service is unavailable.
    #[error("Grok Build session selector is unavailable")]
    Unavailable,
}

/// Selected-session construction failure.
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum GrokSessionDataError {
    /// Access token or verified identity header is malformed.
    #[error("selected session contains an invalid secret value")]
    InvalidSecretValue,
    /// Egress/session binding is invalid.
    #[error("selected session binding is invalid")]
    InvalidBinding,
}

fn valid_secret_header(value: &SecretValue, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value
            .expose()
            .bytes()
            .all(|byte| (0x20..=0x7e).contains(&byte))
}
