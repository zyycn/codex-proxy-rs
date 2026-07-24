//! Codex Authorization Code + PKCE/OIDC 管理流。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, TimeDelta, Utc};
use gateway_admin::model::provider_credentials::{
    AuthorizationMutationTarget, AuthorizationOwner, PendingAuthorizationMutation,
};
use gateway_core::engine::credential::{
    CredentialRevision, NewProviderAccount, ProviderAccountId, ProviderAccountStore,
};
use gateway_core::provider_ports::ProviderRuntimePolicyPort;
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq as _;
use url::Url;
use uuid::Uuid;

use super::admin::{
    CodexCredentialAdmin, CodexCredentialAdminError, ImportCodexOAuthCredential,
    PreparedCodexCredentialRotation, RotateManagedCodexCredential, refresh_time,
};
use super::identity::{
    CodexAccountIdentityVerifier, CodexIdentityExpectation, CodexIdentityVerificationError,
};
use super::security::CodexCredentialCodec;
use super::token_client::{
    AuthorizationCodeExchangeError, AuthorizationCodeExchanger, AuthorizationCodeGrant,
    OFFICIAL_CODEX_OAUTH_CLIENT_ID, OFFICIAL_CODEX_REDIRECT_URI,
};

const AUTHORIZATION_ENDPOINT: &str = "https://auth.openai.com/oauth/authorize";
const AUTHORIZATION_SCOPE: &str = "openid profile email offline_access";
const AUTHORIZATION_TTL: TimeDelta = TimeDelta::minutes(10);
const MAX_CALLBACK_BYTES: usize = 16 * 1024;
const MAX_TEXT_BYTES: usize = 512;

#[derive(Clone, Debug)]
pub struct StartCodexOAuthAuthorization {
    pub mutation: PendingAuthorizationMutation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexOAuthReauthorizationTarget {
    account_id: ProviderAccountId,
    credential_revision: CredentialRevision,
}

impl CodexOAuthReauthorizationTarget {
    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn credential_revision(&self) -> CredentialRevision {
        self.credential_revision
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct CodexOAuthAuthorizationStarted {
    pub flow_id: String,
    pub authorization_url: String,
    pub expires_at: DateTime<Utc>,
}

impl fmt::Debug for CodexOAuthAuthorizationStarted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexOAuthAuthorizationStarted")
            .field("flow_id", &"<redacted>")
            .field("authorization_url", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

pub struct CompleteCodexOAuthAuthorization {
    pub owner_ref: String,
    pub flow_id: String,
    pub callback_url: SecretString,
}

/// OAuth exchange 后返回的 Provider prepared credential 及其原始事务信封。
pub struct CompletedCodexOAuthAuthorization<T> {
    pub mutation: PendingAuthorizationMutation,
    pub credential: T,
}

/// OAuth exchange 后唯一的 credential preparation 结果。
pub enum CompletedCodexOAuthCredential {
    Create(NewProviderAccount),
    Reauthorize(PreparedCodexCredentialRotation),
}

impl fmt::Debug for CompletedCodexOAuthCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Create(_) => formatter.write_str("Create([PREPARED])"),
            Self::Reauthorize(_) => formatter.write_str("Reauthorize([PREPARED])"),
        }
    }
}

impl<T> fmt::Debug for CompletedCodexOAuthAuthorization<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompletedCodexOAuthAuthorization")
            .field("mutation", &self.mutation)
            .field("credential", &"[PREPARED]")
            .finish()
    }
}

impl fmt::Debug for CompleteCodexOAuthAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompleteCodexOAuthAuthorization")
            .field("owner_ref", &"<redacted>")
            .field("flow_id", &"<redacted>")
            .field("callback_url", &"<redacted>")
            .finish()
    }
}

pub struct CodexPendingAuthorization {
    flow_id: String,
    owner_ref: String,
    started_request_ref: String,
    name: String,
    expires_at: DateTime<Utc>,
    state: SecretString,
    nonce: SecretString,
    code_verifier: SecretString,
    reauthorization: Option<CodexOAuthReauthorizationTarget>,
    mutation: PendingAuthorizationMutation,
}

pub struct StoredCodexPendingAuthorization {
    pub flow_id: String,
    pub owner_ref: String,
    pub started_request_ref: String,
    pub name: String,
    pub expires_at: DateTime<Utc>,
    pub state: SecretString,
    pub nonce: SecretString,
    pub code_verifier: SecretString,
    pub reauthorization_account_id: Option<String>,
    pub reauthorization_credential_revision: Option<u64>,
    pub mutation: PendingAuthorizationMutation,
}

impl CodexPendingAuthorization {
    pub fn from_stored(
        input: StoredCodexPendingAuthorization,
    ) -> Result<Self, CodexOAuthPendingStoreError> {
        let reauthorization = match (
            input.reauthorization_account_id,
            input.reauthorization_credential_revision,
        ) {
            (None, None) => None,
            (Some(account_id), Some(revision)) => Some(CodexOAuthReauthorizationTarget {
                account_id: ProviderAccountId::new(account_id)
                    .map_err(|_| CodexOAuthPendingStoreError::InvalidValue)?,
                credential_revision: CredentialRevision::new(revision)
                    .map_err(|_| CodexOAuthPendingStoreError::InvalidValue)?,
            }),
            _ => return Err(CodexOAuthPendingStoreError::InvalidValue),
        };
        let pending = Self {
            flow_id: input.flow_id,
            owner_ref: input.owner_ref,
            started_request_ref: input.started_request_ref,
            name: input.name,
            expires_at: input.expires_at,
            state: input.state,
            nonce: input.nonce,
            code_verifier: input.code_verifier,
            reauthorization,
            mutation: input.mutation,
        };
        if !valid_text(&pending.flow_id)
            || !valid_text(&pending.owner_ref)
            || !valid_text(&pending.started_request_ref)
            || !valid_text(&pending.name)
            || pending.expires_at <= Utc::now()
            || !valid_secret(pending.state.expose_secret())
            || !valid_secret(pending.nonce.expose_secret())
            || !valid_secret(pending.code_verifier.expose_secret())
            || !pending_mutation_matches(&pending)
        {
            return Err(CodexOAuthPendingStoreError::InvalidValue);
        }
        Ok(pending)
    }

    #[must_use]
    pub fn flow_id(&self) -> &str {
        &self.flow_id
    }

    #[must_use]
    pub fn owner_ref(&self) -> &str {
        &self.owner_ref
    }

    #[must_use]
    pub fn started_request_ref(&self) -> &str {
        &self.started_request_ref
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }

    #[must_use]
    pub const fn state(&self) -> &SecretString {
        &self.state
    }

    #[must_use]
    pub const fn nonce(&self) -> &SecretString {
        &self.nonce
    }

    #[must_use]
    pub const fn code_verifier(&self) -> &SecretString {
        &self.code_verifier
    }

    #[must_use]
    pub const fn reauthorization(&self) -> Option<&CodexOAuthReauthorizationTarget> {
        self.reauthorization.as_ref()
    }

    #[must_use]
    pub const fn mutation(&self) -> &PendingAuthorizationMutation {
        &self.mutation
    }
}

impl fmt::Debug for CodexPendingAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexPendingAuthorization")
            .field("flow_id", &"<redacted>")
            .field("owner_ref", &"<redacted>")
            .field("started_request_ref", &"<redacted>")
            .field("name", &self.name)
            .field("expires_at", &self.expires_at)
            .field("state", &"<redacted>")
            .field("nonce", &"<redacted>")
            .field("code_verifier", &"<redacted>")
            .field("reauthorization", &self.reauthorization)
            .field("mutation", &self.mutation)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CodexOAuthPendingStoreError {
    #[error("pending Codex OAuth flow is invalid")]
    InvalidValue,
    #[error("pending Codex OAuth flow already exists")]
    Conflict,
    #[error("pending Codex OAuth flow store is unavailable")]
    Unavailable,
}

#[async_trait]
pub trait CodexOAuthPendingStore: Send + Sync {
    async fn create(
        &self,
        pending: &CodexPendingAuthorization,
    ) -> Result<(), CodexOAuthPendingStoreError>;

    async fn take(
        &self,
        owner_ref: &str,
        flow_id: &str,
    ) -> Result<Option<CodexPendingAuthorization>, CodexOAuthPendingStoreError>;
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CodexOAuthAdminError {
    #[error("invalid Codex OAuth admin input")]
    InvalidInput,
    #[error("Codex OAuth flow was not found")]
    NotFound,
    #[error("Codex OAuth operation conflicts with current state")]
    Conflict,
    #[error("Codex OAuth flow expired")]
    FlowExpired,
    #[error("Codex OAuth upstream rejected the operation")]
    UpstreamRejected,
    #[error("Codex OAuth upstream is unavailable")]
    UpstreamUnavailable,
    #[error("Codex OAuth exchange send state is ambiguous")]
    Ambiguous,
    #[error("Codex OAuth pending storage is unavailable")]
    StorageUnavailable,
    #[error("Codex OAuth account mutation failed")]
    Credential,
}

#[async_trait]
pub trait CodexOAuthAdmin: Send + Sync {
    async fn start_authorization(
        &self,
        command: StartCodexOAuthAuthorization,
    ) -> Result<CodexOAuthAuthorizationStarted, CodexOAuthAdminError>;

    async fn complete_authorization(
        &self,
        command: CompleteCodexOAuthAuthorization,
    ) -> Result<CompletedCodexOAuthAuthorization<CompletedCodexOAuthCredential>, CodexOAuthAdminError>;
}

pub struct CodexOAuthAdminService {
    pending: Arc<dyn CodexOAuthPendingStore>,
    exchanger: Arc<dyn AuthorizationCodeExchanger>,
    verifier: Arc<dyn CodexAccountIdentityVerifier>,
    store: Arc<dyn ProviderAccountStore>,
    runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
    credentials: CodexCredentialAdmin,
}

impl CodexOAuthAdminService {
    #[must_use]
    pub const fn new(
        pending: Arc<dyn CodexOAuthPendingStore>,
        exchanger: Arc<dyn AuthorizationCodeExchanger>,
        verifier: Arc<dyn CodexAccountIdentityVerifier>,
        store: Arc<dyn ProviderAccountStore>,
        runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
        credentials: CodexCredentialAdmin,
    ) -> Self {
        Self {
            pending,
            exchanger,
            verifier,
            store,
            runtime_policy,
            credentials,
        }
    }
}

#[async_trait]
impl CodexOAuthAdmin for CodexOAuthAdminService {
    async fn start_authorization(
        &self,
        command: StartCodexOAuthAuthorization,
    ) -> Result<CodexOAuthAuthorizationStarted, CodexOAuthAdminError> {
        let (name, reauthorization) = match command.mutation.target() {
            AuthorizationMutationTarget::Create { name } => (name.clone(), None),
            AuthorizationMutationTarget::Reauthorize {
                account_id,
                expected_credential_revision,
            } => {
                let expected_revision = CredentialRevision::new(expected_credential_revision.get())
                    .map_err(|_| CodexOAuthAdminError::InvalidInput)?;
                let current = self
                    .store
                    .load_credential(account_id, expected_revision)
                    .await
                    .map_err(map_store_error)?;
                if current.account.provider().as_str() != "openai"
                    || current.account.id() != account_id
                {
                    return Err(CodexOAuthAdminError::NotFound);
                }
                (
                    current.account.name().to_owned(),
                    Some(CodexOAuthReauthorizationTarget {
                        account_id: account_id.clone(),
                        credential_revision: expected_revision,
                    }),
                )
            }
        };
        self.start_pending(command.mutation, name, reauthorization)
            .await
    }

    async fn complete_authorization(
        &self,
        command: CompleteCodexOAuthAuthorization,
    ) -> Result<CompletedCodexOAuthAuthorization<CompletedCodexOAuthCredential>, CodexOAuthAdminError>
    {
        if !valid_text(&command.owner_ref)
            || !valid_text(&command.flow_id)
            || command.callback_url.expose_secret().len() > MAX_CALLBACK_BYTES
        {
            return Err(CodexOAuthAdminError::InvalidInput);
        }
        let (pending, mut secret, id_token) = self.exchange_pending(command).await?;
        let mutation = pending.mutation.clone();
        let current = if let Some(target) = pending.reauthorization() {
            let current = self
                .store
                .load_credential(target.account_id(), target.credential_revision())
                .await
                .map_err(map_store_error)?;
            Some(current)
        } else {
            None
        };
        let expectation = current
            .as_ref()
            .map(oauth_identity_expectation)
            .transpose()?
            .unwrap_or_default();
        let profile = self
            .verifier
            .verify_authorization(&secret, &id_token, &pending.nonce, &expectation)
            .await
            .and_then(super::identity::CodexIdentityVerification::into_complete)
            .map_err(map_identity_error)?;
        secret.id_token = Some(id_token);
        let policy = self
            .runtime_policy
            .load_refresh_policy()
            .await
            .map_err(|_| CodexOAuthAdminError::StorageUnavailable)?;
        let credential = if let Some(current) = current {
            let next_refresh_at = refresh_time(
                policy,
                current.account.id(),
                profile.access_token_expires_at,
                secret.refresh_token.is_some(),
            )
            .map_err(map_admin_error)?;
            CompletedCodexOAuthCredential::Reauthorize(
                self.credentials
                    .prepare_rotation(RotateManagedCodexCredential {
                        current,
                        secret,
                        verified_account: profile,
                        next_refresh_at,
                    })
                    .map_err(map_admin_error)?,
            )
        } else {
            let account_id = format!("acct_{}", Uuid::now_v7().simple());
            let typed_account_id = ProviderAccountId::new(account_id.clone())
                .map_err(|_| CodexOAuthAdminError::Credential)?;
            let next_refresh_at = refresh_time(
                policy,
                &typed_account_id,
                profile.access_token_expires_at,
                secret.refresh_token.is_some(),
            )
            .map_err(map_admin_error)?;
            CompletedCodexOAuthCredential::Create(
                self.credentials
                    .prepare_import(ImportCodexOAuthCredential {
                        account_id,
                        name: pending.name,
                        secret,
                        verified_account: profile,
                        next_refresh_at,
                        enabled: true,
                    })
                    .map_err(map_admin_error)?,
            )
        };
        Ok(CompletedCodexOAuthAuthorization {
            mutation,
            credential,
        })
    }
}

impl CodexOAuthAdminService {
    async fn start_pending(
        &self,
        mutation: PendingAuthorizationMutation,
        name: String,
        reauthorization: Option<CodexOAuthReauthorizationTarget>,
    ) -> Result<CodexOAuthAuthorizationStarted, CodexOAuthAdminError> {
        if !valid_text(&name) {
            return Err(CodexOAuthAdminError::InvalidInput);
        }
        let expires_at = Utc::now()
            .checked_add_signed(AUTHORIZATION_TTL)
            .ok_or(CodexOAuthAdminError::InvalidInput)?;
        let owner_ref = oauth_owner_ref(mutation.owner_binding().owner());
        let started_request_ref = mutation.owner_binding().started_request_id().to_owned();
        let pending = CodexPendingAuthorization::from_stored(StoredCodexPendingAuthorization {
            flow_id: random_secret()?,
            owner_ref,
            started_request_ref,
            name,
            expires_at,
            state: SecretString::from(random_secret()?),
            nonce: SecretString::from(random_secret()?),
            code_verifier: SecretString::from(random_secret()?),
            reauthorization_account_id: reauthorization
                .as_ref()
                .map(|target| target.account_id().to_string()),
            reauthorization_credential_revision: reauthorization
                .map(|target| target.credential_revision().get()),
            mutation,
        })
        .map_err(map_pending_error)?;
        self.pending
            .create(&pending)
            .await
            .map_err(map_pending_error)?;
        Ok(CodexOAuthAuthorizationStarted {
            flow_id: pending.flow_id.clone(),
            authorization_url: authorization_url(&pending)?,
            expires_at,
        })
    }

    async fn exchange_pending(
        &self,
        command: CompleteCodexOAuthAuthorization,
    ) -> Result<
        (
            CodexPendingAuthorization,
            super::types::CodexOAuthSecret,
            SecretString,
        ),
        CodexOAuthAdminError,
    > {
        if !valid_text(&command.owner_ref)
            || !valid_text(&command.flow_id)
            || command.callback_url.expose_secret().len() > MAX_CALLBACK_BYTES
        {
            return Err(CodexOAuthAdminError::InvalidInput);
        }
        let pending = self
            .pending
            .take(&command.owner_ref, &command.flow_id)
            .await
            .map_err(map_pending_error)?
            .ok_or(CodexOAuthAdminError::NotFound)?;
        if pending.expires_at <= Utc::now() {
            return Err(CodexOAuthAdminError::FlowExpired);
        }
        let (code, callback_state) = callback_parts(command.callback_url.expose_secret())?;
        if !constant_time_equal(
            pending.state.expose_secret().as_bytes(),
            callback_state.expose_secret().as_bytes(),
        ) {
            return Err(CodexOAuthAdminError::UpstreamRejected);
        }
        let tokens = self
            .exchanger
            .exchange_authorization_code(AuthorizationCodeGrant {
                code,
                code_verifier: pending.code_verifier.clone(),
            })
            .await
            .map_err(map_exchange_error)?;
        Ok((pending, tokens.secret, tokens.id_token))
    }
}

fn oauth_identity_expectation(
    current: &gateway_core::engine::credential::LoadedCredential,
) -> Result<CodexIdentityExpectation, CodexOAuthAdminError> {
    let runtime = CodexCredentialCodec::decode(&current.credential)
        .map_err(|_| CodexOAuthAdminError::Credential)?;
    let account_id = current
        .account
        .upstream_account_id()
        .ok_or(CodexOAuthAdminError::Credential)?;
    let principal = runtime.principal.ok_or(CodexOAuthAdminError::Credential)?;
    CodexIdentityExpectation::current(
        principal.oauth_subject,
        principal.poid,
        account_id.to_owned(),
        current.account.upstream_user_id().to_owned(),
        runtime.installation_id,
    )
    .map_err(|_| CodexOAuthAdminError::Credential)
}

fn authorization_url(pending: &CodexPendingAuthorization) -> Result<String, CodexOAuthAdminError> {
    let mut url =
        Url::parse(AUTHORIZATION_ENDPOINT).map_err(|_| CodexOAuthAdminError::InvalidInput)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(
        pending.code_verifier.expose_secret().as_bytes(),
    ));
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", OFFICIAL_CODEX_OAUTH_CLIENT_ID)
        .append_pair("redirect_uri", OFFICIAL_CODEX_REDIRECT_URI)
        .append_pair("scope", AUTHORIZATION_SCOPE)
        .append_pair("state", pending.state.expose_secret())
        .append_pair("nonce", pending.nonce.expose_secret())
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true");
    Ok(url.into())
}

fn callback_parts(value: &str) -> Result<(SecretString, SecretString), CodexOAuthAdminError> {
    let url = Url::parse(value).map_err(|_| CodexOAuthAdminError::UpstreamRejected)?;
    if url.scheme() != "http"
        || url.host_str() != Some("localhost")
        || url.port() != Some(1455)
        || url.path() != "/auth/callback"
        || url.fragment().is_some()
    {
        return Err(CodexOAuthAdminError::UpstreamRejected);
    }
    let mut code = None;
    let mut state = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" if code.is_none() => code = Some(value.into_owned()),
            "state" if state.is_none() => state = Some(value.into_owned()),
            _ => return Err(CodexOAuthAdminError::UpstreamRejected),
        }
    }
    let code = code.filter(|value| valid_secret(value));
    let state = state.filter(|value| valid_secret(value));
    match (code, state) {
        (Some(code), Some(state)) => Ok((SecretString::from(code), SecretString::from(state))),
        _ => Err(CodexOAuthAdminError::UpstreamRejected),
    }
}

fn random_secret() -> Result<String, CodexOAuthAdminError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| CodexOAuthAdminError::StorageUnavailable)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_TEXT_BYTES && !value.chars().any(char::is_control)
}

fn valid_secret(value: &str) -> bool {
    valid_text(value) && value.len() >= 16
}

pub(crate) fn oauth_owner_ref(owner: &AuthorizationOwner) -> String {
    let mut digest = Sha256::new();
    match owner {
        AuthorizationOwner::AdminSession { admin_user_id } => {
            digest.update(b"admin-session\0");
            digest.update(admin_user_id.as_bytes());
        }
        AuthorizationOwner::AdminApiKey => digest.update(b"admin-api-key"),
        AuthorizationOwner::System => digest.update(b"system"),
    }
    URL_SAFE_NO_PAD.encode(digest.finalize())
}

fn pending_mutation_matches(pending: &CodexPendingAuthorization) -> bool {
    if pending.mutation.provider_kind().as_str() != "openai"
        || pending.mutation.owner_binding().started_request_id() != pending.started_request_ref
        || oauth_owner_ref(pending.mutation.owner_binding().owner()) != pending.owner_ref
    {
        return false;
    }
    match (pending.mutation.target(), pending.reauthorization.as_ref()) {
        (AuthorizationMutationTarget::Create { name }, None) => name == &pending.name,
        (
            AuthorizationMutationTarget::Reauthorize {
                account_id,
                expected_credential_revision,
            },
            Some(target),
        ) => {
            account_id == target.account_id()
                && expected_credential_revision.get() == target.credential_revision().get()
        }
        _ => false,
    }
}

fn map_pending_error(error: CodexOAuthPendingStoreError) -> CodexOAuthAdminError {
    match error {
        CodexOAuthPendingStoreError::InvalidValue => CodexOAuthAdminError::InvalidInput,
        CodexOAuthPendingStoreError::Conflict => CodexOAuthAdminError::Conflict,
        CodexOAuthPendingStoreError::Unavailable => CodexOAuthAdminError::StorageUnavailable,
    }
}

fn map_exchange_error(error: AuthorizationCodeExchangeError) -> CodexOAuthAdminError {
    match error {
        AuthorizationCodeExchangeError::Rejected => CodexOAuthAdminError::UpstreamRejected,
        AuthorizationCodeExchangeError::Unavailable => CodexOAuthAdminError::UpstreamUnavailable,
        AuthorizationCodeExchangeError::Ambiguous => CodexOAuthAdminError::Ambiguous,
    }
}

fn map_identity_error(_: CodexIdentityVerificationError) -> CodexOAuthAdminError {
    CodexOAuthAdminError::UpstreamRejected
}

fn map_admin_error(_: CodexCredentialAdminError) -> CodexOAuthAdminError {
    CodexOAuthAdminError::Credential
}

fn map_store_error(error: gateway_core::error::StoreError) -> CodexOAuthAdminError {
    match error.kind() {
        gateway_core::error::StoreErrorKind::Conflict => CodexOAuthAdminError::Conflict,
        gateway_core::error::StoreErrorKind::InvalidData
        | gateway_core::error::StoreErrorKind::InvalidState => CodexOAuthAdminError::NotFound,
        gateway_core::error::StoreErrorKind::Unavailable => {
            CodexOAuthAdminError::StorageUnavailable
        }
        _ => CodexOAuthAdminError::StorageUnavailable,
    }
}
