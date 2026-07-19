//! Codex Authorization Code + PKCE/OIDC 管理流。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, TimeDelta, Utc};
use gateway_core::engine::credential::{
    CredentialRevision, NewProviderAccount, ProviderAccountId, ProviderAccountStore,
};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq as _;
use url::Url;
use uuid::Uuid;

use super::admin::{
    CodexCredentialAdmin, CodexCredentialAdminError, ImportCodexOAuthCredential,
    PreparedCodexCredentialRotation, RotateManagedCodexCredential,
};
use super::identity::{CodexAuthorizationTokenVerifier, CodexIdentityVerificationError};
use super::token_client::{
    AuthorizationCodeExchangeError, AuthorizationCodeExchanger, AuthorizationCodeGrant,
    OFFICIAL_CODEX_OAUTH_CLIENT_ID, OFFICIAL_CODEX_REDIRECT_URI,
};

const AUTHORIZATION_ENDPOINT: &str = "https://auth.openai.com/oauth/authorize";
const AUTHORIZATION_SCOPE: &str = "openid profile email offline_access";
const AUTHORIZATION_TTL: TimeDelta = TimeDelta::minutes(10);
const MAX_CALLBACK_BYTES: usize = 16 * 1024;
const MAX_TEXT_BYTES: usize = 512;

#[derive(Clone)]
pub struct CodexOAuthFlowBinding {
    owner_ref: String,
    started_request_ref: String,
}

impl CodexOAuthFlowBinding {
    pub fn new(
        owner_ref: impl Into<String>,
        started_request_ref: impl Into<String>,
    ) -> Result<Self, CodexOAuthAdminError> {
        let binding = Self {
            owner_ref: owner_ref.into(),
            started_request_ref: started_request_ref.into(),
        };
        if !valid_text(&binding.owner_ref) || !valid_text(&binding.started_request_ref) {
            return Err(CodexOAuthAdminError::InvalidInput);
        }
        Ok(binding)
    }
}

impl fmt::Debug for CodexOAuthFlowBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexOAuthFlowBinding")
            .field("owner_ref", &"<redacted>")
            .field("started_request_ref", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct StartCodexOAuthAuthorization {
    pub binding: CodexOAuthFlowBinding,
    pub provider_instance_id: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct StartCodexOAuthReauthorization {
    pub binding: CodexOAuthFlowBinding,
    pub account_id: ProviderAccountId,
    pub expected_credential_revision: CredentialRevision,
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
    provider_instance_id: String,
    name: String,
    expires_at: DateTime<Utc>,
    state: SecretString,
    nonce: SecretString,
    code_verifier: SecretString,
    reauthorization: Option<CodexOAuthReauthorizationTarget>,
}

pub struct StoredCodexPendingAuthorization {
    pub flow_id: String,
    pub owner_ref: String,
    pub started_request_ref: String,
    pub provider_instance_id: String,
    pub name: String,
    pub expires_at: DateTime<Utc>,
    pub state: SecretString,
    pub nonce: SecretString,
    pub code_verifier: SecretString,
    pub reauthorization_account_id: Option<String>,
    pub reauthorization_credential_revision: Option<u64>,
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
            provider_instance_id: input.provider_instance_id,
            name: input.name,
            expires_at: input.expires_at,
            state: input.state,
            nonce: input.nonce,
            code_verifier: input.code_verifier,
            reauthorization,
        };
        if !valid_text(&pending.flow_id)
            || !valid_text(&pending.owner_ref)
            || !valid_text(&pending.started_request_ref)
            || !valid_text(&pending.provider_instance_id)
            || !valid_text(&pending.name)
            || pending.expires_at <= Utc::now()
            || !valid_secret(pending.state.expose_secret())
            || !valid_secret(pending.nonce.expose_secret())
            || !valid_secret(pending.code_verifier.expose_secret())
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
    pub fn provider_instance_id(&self) -> &str {
        &self.provider_instance_id
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
}

impl fmt::Debug for CodexPendingAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexPendingAuthorization")
            .field("flow_id", &"<redacted>")
            .field("owner_ref", &"<redacted>")
            .field("started_request_ref", &"<redacted>")
            .field("provider_instance_id", &self.provider_instance_id)
            .field("name", &self.name)
            .field("expires_at", &self.expires_at)
            .field("state", &"<redacted>")
            .field("nonce", &"<redacted>")
            .field("code_verifier", &"<redacted>")
            .field("reauthorization", &self.reauthorization)
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
    ) -> Result<NewProviderAccount, CodexOAuthAdminError>;

    async fn start_reauthorization(
        &self,
        command: StartCodexOAuthReauthorization,
    ) -> Result<CodexOAuthAuthorizationStarted, CodexOAuthAdminError>;

    async fn complete_reauthorization(
        &self,
        command: CompleteCodexOAuthAuthorization,
    ) -> Result<PreparedCodexCredentialRotation, CodexOAuthAdminError>;
}

pub struct CodexOAuthAdminService {
    pending: Arc<dyn CodexOAuthPendingStore>,
    exchanger: Arc<dyn AuthorizationCodeExchanger>,
    verifier: Arc<dyn CodexAuthorizationTokenVerifier>,
    store: Arc<dyn ProviderAccountStore>,
    credentials: CodexCredentialAdmin,
}

impl CodexOAuthAdminService {
    #[must_use]
    pub const fn new(
        pending: Arc<dyn CodexOAuthPendingStore>,
        exchanger: Arc<dyn AuthorizationCodeExchanger>,
        verifier: Arc<dyn CodexAuthorizationTokenVerifier>,
        store: Arc<dyn ProviderAccountStore>,
        credentials: CodexCredentialAdmin,
    ) -> Self {
        Self {
            pending,
            exchanger,
            verifier,
            store,
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
        self.start_pending(
            command.binding,
            command.provider_instance_id,
            command.name,
            None,
        )
        .await
    }

    async fn complete_authorization(
        &self,
        command: CompleteCodexOAuthAuthorization,
    ) -> Result<NewProviderAccount, CodexOAuthAdminError> {
        if !valid_text(&command.owner_ref)
            || !valid_text(&command.flow_id)
            || command.callback_url.expose_secret().len() > MAX_CALLBACK_BYTES
        {
            return Err(CodexOAuthAdminError::InvalidInput);
        }
        let pending = self.exchange_pending(command).await?;
        if pending.0.reauthorization().is_some() {
            return Err(CodexOAuthAdminError::Conflict);
        }
        let (pending, secret, profile) = pending;
        self.credentials
            .prepare_import(ImportCodexOAuthCredential {
                account_id: format!("acct_{}", Uuid::now_v7().simple()),
                provider_instance_id: pending.provider_instance_id,
                name: pending.name,
                secret,
                verified_account: profile,
                enabled: true,
            })
            .map_err(map_admin_error)
    }

    async fn start_reauthorization(
        &self,
        command: StartCodexOAuthReauthorization,
    ) -> Result<CodexOAuthAuthorizationStarted, CodexOAuthAdminError> {
        let current = self
            .store
            .load_credential(&command.account_id, command.expected_credential_revision)
            .await
            .map_err(map_store_error)?;
        if current.account.provider().as_str() != "openai"
            || current.account.id() != &command.account_id
        {
            return Err(CodexOAuthAdminError::NotFound);
        }
        self.start_pending(
            command.binding,
            current.account.instance().to_string(),
            current.account.name().to_owned(),
            Some(CodexOAuthReauthorizationTarget {
                account_id: command.account_id,
                credential_revision: command.expected_credential_revision,
            }),
        )
        .await
    }

    async fn complete_reauthorization(
        &self,
        command: CompleteCodexOAuthAuthorization,
    ) -> Result<PreparedCodexCredentialRotation, CodexOAuthAdminError> {
        let (pending, secret, profile) = self.exchange_pending(command).await?;
        let target = pending
            .reauthorization()
            .ok_or(CodexOAuthAdminError::Conflict)?;
        let current = self
            .store
            .load_credential(target.account_id(), target.credential_revision())
            .await
            .map_err(map_store_error)?;
        self.credentials
            .prepare_rotation(RotateManagedCodexCredential {
                current,
                secret,
                verified_account: profile,
            })
            .map_err(map_admin_error)
    }
}

impl CodexOAuthAdminService {
    async fn start_pending(
        &self,
        binding: CodexOAuthFlowBinding,
        provider_instance_id: String,
        name: String,
        reauthorization: Option<CodexOAuthReauthorizationTarget>,
    ) -> Result<CodexOAuthAuthorizationStarted, CodexOAuthAdminError> {
        if !valid_text(&provider_instance_id) || !valid_text(&name) {
            return Err(CodexOAuthAdminError::InvalidInput);
        }
        let expires_at = Utc::now()
            .checked_add_signed(AUTHORIZATION_TTL)
            .ok_or(CodexOAuthAdminError::InvalidInput)?;
        let pending = CodexPendingAuthorization::from_stored(StoredCodexPendingAuthorization {
            flow_id: random_secret()?,
            owner_ref: binding.owner_ref,
            started_request_ref: binding.started_request_ref,
            provider_instance_id,
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
            super::types::CodexAccountProfile,
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
        let mut tokens = self
            .exchanger
            .exchange_authorization_code(AuthorizationCodeGrant {
                code,
                code_verifier: pending.code_verifier.clone(),
            })
            .await
            .map_err(map_exchange_error)?;
        let profile = self
            .verifier
            .verify_authorization(&tokens.secret, &tokens.id_token, &pending.nonce)
            .await
            .map_err(map_identity_error)?;
        tokens.secret.id_token = Some(tokens.id_token);
        Ok((pending, tokens.secret, profile))
    }
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
