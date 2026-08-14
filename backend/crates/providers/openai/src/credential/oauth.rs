//! Codex Authorization Code + PKCE/OIDC 管理流。

use std::{
    fmt,
    sync::Arc,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, TimeDelta, Utc};
use gateway_admin::model::{
    AdminError,
    provider_credentials::{
        AuthorizationCommitGuard, AuthorizationMutationTarget, AuthorizationOwner,
        PendingAuthorizationMutation,
    },
};
use gateway_core::engine::credential::{
    LoadedCredential, NewProviderAccount, ProviderAccountId, ProviderAccountStore,
};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq as _;
use url::Url;
use uuid::Uuid;

use super::admin::{
    CodexCredentialAdmin, CodexCredentialAdminError, PreparedCodexCredentialRotation,
    UnresolvedCodexOAuthCredential,
};
use super::recovery_log::{CodexOAuthRecoveryOperation, record_oauth_recovery};
use super::security::CodexCredentialCodec;
use super::token_client::{
    AuthorizationCodeExchangeError, AuthorizationCodeExchanger, AuthorizationCodeGrant,
    OFFICIAL_CODEX_OAUTH_CLIENT_ID, OFFICIAL_CODEX_REDIRECT_URI,
};
use super::types::parse_chatgpt_jwt_claims;

const AUTHORIZATION_ENDPOINT: &str = "https://auth.openai.com/oauth/authorize";
const AUTHORIZATION_SCOPE: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
const AUTHORIZATION_ORIGINATOR: &str = "Codex Desktop";
const AUTHORIZATION_TTL: TimeDelta = TimeDelta::minutes(10);
const AUTHORIZATION_CLAIM_TTL: Duration = Duration::from_secs(90);
const MAX_CALLBACK_BYTES: usize = 16 * 1024;
const MAX_TEXT_BYTES: usize = 512;

#[derive(Clone, Debug)]
pub struct StartCodexOAuthAuthorization {
    pub mutation: PendingAuthorizationMutation,
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
    authorization_guard: Box<dyn AuthorizationCommitGuard>,
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

impl<T> CompletedCodexOAuthAuthorization<T> {
    pub(crate) fn into_parts(
        self,
    ) -> (
        PendingAuthorizationMutation,
        T,
        Box<dyn AuthorizationCommitGuard>,
    ) {
        (self.mutation, self.credential, self.authorization_guard)
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
    reauthorization: Option<ProviderAccountId>,
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
    pub mutation: PendingAuthorizationMutation,
}

impl CodexPendingAuthorization {
    pub fn from_stored(
        input: StoredCodexPendingAuthorization,
    ) -> Result<Self, CodexOAuthPendingStoreError> {
        let reauthorization = input
            .reauthorization_account_id
            .map(ProviderAccountId::new)
            .transpose()
            .map_err(|_| CodexOAuthPendingStoreError::InvalidValue)?;
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
    pub const fn reauthorization(&self) -> Option<&ProviderAccountId> {
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

    async fn claim(
        &self,
        owner_ref: &str,
        flow_id: &str,
        claim_ref: &str,
        claim_ttl: Duration,
    ) -> Result<CodexOAuthPendingClaimOutcome, CodexOAuthPendingStoreError>;

    async fn release_claim(
        &self,
        owner_ref: &str,
        flow_id: &str,
        claim_ref: &str,
    ) -> Result<bool, CodexOAuthPendingStoreError>;

    async fn consume_claim(
        &self,
        owner_ref: &str,
        flow_id: &str,
        claim_ref: &str,
    ) -> Result<bool, CodexOAuthPendingStoreError>;
}

pub enum CodexOAuthPendingClaimOutcome {
    Claimed(Box<CodexPendingAuthorization>),
    NotFound,
    InProgress,
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
    #[error("Codex OAuth callback was rejected")]
    CallbackRejected,
    #[error("Codex OAuth token exchange was rejected")]
    TokenRejected,
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
    store: Arc<dyn ProviderAccountStore>,
    credentials: CodexCredentialAdmin,
    oauth_client_id: String,
}

struct CodexOAuthAuthorizationCommitGuard {
    pending: Arc<dyn CodexOAuthPendingStore>,
    owner_ref: String,
    flow_id: String,
    claim_ref: String,
}

#[async_trait]
impl AuthorizationCommitGuard for CodexOAuthAuthorizationCommitGuard {
    async fn commit(self: Box<Self>) -> Result<(), AdminError> {
        let consumed = self
            .pending
            .consume_claim(&self.owner_ref, &self.flow_id, &self.claim_ref)
            .await
            .map_err(map_authorization_settlement_error)?;
        if consumed {
            Ok(())
        } else {
            Err(AdminError::conflict(
                "OpenAI OAuth pending claim is no longer current",
            ))
        }
    }

    async fn abort(self: Box<Self>) -> Result<(), AdminError> {
        let released = self
            .pending
            .release_claim(&self.owner_ref, &self.flow_id, &self.claim_ref)
            .await
            .map_err(map_authorization_settlement_error)?;
        if released {
            Ok(())
        } else {
            Err(AdminError::unavailable(
                "OpenAI OAuth pending claim could not be released",
            ))
        }
    }
}

impl CodexOAuthAdminService {
    #[must_use]
    pub fn new(
        pending: Arc<dyn CodexOAuthPendingStore>,
        exchanger: Arc<dyn AuthorizationCodeExchanger>,
        store: Arc<dyn ProviderAccountStore>,
        credentials: CodexCredentialAdmin,
    ) -> Self {
        Self {
            pending,
            exchanger,
            store,
            credentials,
            oauth_client_id: OFFICIAL_CODEX_OAUTH_CLIENT_ID.to_owned(),
        }
    }

    #[must_use]
    pub fn with_oauth_client_id(mut self, oauth_client_id: impl Into<String>) -> Self {
        self.oauth_client_id = oauth_client_id.into();
        self
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
            AuthorizationMutationTarget::Reauthorize { account_id } => {
                let current = self
                    .store
                    .load_current_credential(account_id)
                    .await
                    .map_err(map_store_error)?;
                if current.account.provider().as_str() != "openai"
                    || current.account.id() != account_id
                {
                    return Err(CodexOAuthAdminError::NotFound);
                }
                (current.account.name().to_owned(), Some(account_id.clone()))
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
        let claim_ref = random_secret()?;
        let pending = self.claim_pending(&command, &claim_ref).await?;
        let completed = self
            .complete_claimed_authorization(pending, command.callback_url.expose_secret())
            .await;
        match completed {
            Ok((mutation, credential)) => Ok(CompletedCodexOAuthAuthorization {
                mutation,
                credential,
                authorization_guard: Box::new(CodexOAuthAuthorizationCommitGuard {
                    pending: Arc::clone(&self.pending),
                    owner_ref: command.owner_ref,
                    flow_id: command.flow_id,
                    claim_ref,
                }),
            }),
            Err(error) => {
                self.release_claim(&command, &claim_ref).await?;
                Err(error)
            }
        }
    }
}

impl CodexOAuthAdminService {
    async fn complete_claimed_authorization(
        &self,
        pending: CodexPendingAuthorization,
        callback_url: &str,
    ) -> Result<(PendingAuthorizationMutation, CompletedCodexOAuthCredential), CodexOAuthAdminError>
    {
        let current = if let Some(target) = pending.reauthorization() {
            let current = self
                .store
                .load_current_credential(target)
                .await
                .map_err(map_store_error)?;
            Some(current)
        } else {
            None
        };
        let fallback_refresh_token = current
            .as_ref()
            .map(current_oauth_refresh_token)
            .transpose()?
            .flatten();
        let (mut secret, id_token, expires_in) = self
            .exchange_pending(&pending, callback_url, fallback_refresh_token)
            .await?;
        let mutation = pending.mutation.clone();
        let access_token_expires_at = expires_in
            .and_then(|expires_in| SystemTime::now().checked_add(expires_in))
            .map(DateTime::<Utc>::from);
        let metadata = parse_chatgpt_jwt_claims(id_token.expose_secret())
            .map_err(|_| CodexOAuthAdminError::TokenRejected)?;
        secret.id_token = Some(id_token);
        let credential = if let Some(current) = current {
            CompletedCodexOAuthCredential::Reauthorize(
                self.credentials
                    .prepare_refreshed_oauth_rotation(
                        current,
                        secret,
                        access_token_expires_at,
                        None,
                    )
                    .map_err(map_admin_error)?,
            )
        } else {
            let account_id = format!("acct_{}", Uuid::now_v7().simple());
            CompletedCodexOAuthCredential::Create(
                self.credentials
                    .prepare_unresolved_oauth(UnresolvedCodexOAuthCredential {
                        account_id,
                        name: pending.name,
                        secret,
                        metadata,
                        access_token_expires_at,
                        next_refresh_at: None,
                        enabled: true,
                    })
                    .map_err(map_admin_error)?,
            )
        };
        Ok((mutation, credential))
    }
}

impl CodexOAuthAdminService {
    async fn start_pending(
        &self,
        mutation: PendingAuthorizationMutation,
        name: String,
        reauthorization: Option<ProviderAccountId>,
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
            reauthorization_account_id: reauthorization.as_ref().map(ToString::to_string),
            mutation,
        })
        .map_err(map_pending_error)?;
        self.pending
            .create(&pending)
            .await
            .map_err(map_pending_error)?;
        Ok(CodexOAuthAuthorizationStarted {
            flow_id: pending.flow_id.clone(),
            authorization_url: authorization_url(&pending, &self.oauth_client_id)?,
            expires_at,
        })
    }

    async fn claim_pending(
        &self,
        command: &CompleteCodexOAuthAuthorization,
        claim_ref: &str,
    ) -> Result<CodexPendingAuthorization, CodexOAuthAdminError> {
        if !valid_text(&command.owner_ref)
            || !valid_text(&command.flow_id)
            || !valid_secret(claim_ref)
            || command.callback_url.expose_secret().len() > MAX_CALLBACK_BYTES
        {
            return Err(CodexOAuthAdminError::InvalidInput);
        }
        let pending = match self
            .pending
            .claim(
                &command.owner_ref,
                &command.flow_id,
                claim_ref,
                AUTHORIZATION_CLAIM_TTL,
            )
            .await
            .map_err(map_pending_error)?
        {
            CodexOAuthPendingClaimOutcome::Claimed(pending) => *pending,
            CodexOAuthPendingClaimOutcome::NotFound => return Err(CodexOAuthAdminError::NotFound),
            CodexOAuthPendingClaimOutcome::InProgress => {
                return Err(CodexOAuthAdminError::Conflict);
            }
        };
        if pending.expires_at <= Utc::now() {
            self.release_claim(command, claim_ref).await?;
            return Err(CodexOAuthAdminError::FlowExpired);
        }
        Ok(pending)
    }

    async fn release_claim(
        &self,
        command: &CompleteCodexOAuthAuthorization,
        claim_ref: &str,
    ) -> Result<(), CodexOAuthAdminError> {
        if self
            .pending
            .release_claim(&command.owner_ref, &command.flow_id, claim_ref)
            .await
            .map_err(map_pending_error)?
        {
            Ok(())
        } else {
            Err(CodexOAuthAdminError::StorageUnavailable)
        }
    }

    async fn exchange_pending(
        &self,
        pending: &CodexPendingAuthorization,
        callback_url: &str,
        fallback_refresh_token: Option<SecretString>,
    ) -> Result<
        (
            super::types::CodexOAuthSecret,
            SecretString,
            Option<Duration>,
        ),
        CodexOAuthAdminError,
    > {
        let (code, callback_state) = callback_parts(callback_url)?;
        if !constant_time_equal(
            pending.state.expose_secret().as_bytes(),
            callback_state.expose_secret().as_bytes(),
        ) {
            return Err(CodexOAuthAdminError::CallbackRejected);
        }
        let tokens = self
            .exchanger
            .exchange_authorization_code(AuthorizationCodeGrant {
                code,
                code_verifier: pending.code_verifier.clone(),
            })
            .await
            .map_err(map_exchange_error)?;
        let mut secret = tokens.secret;
        if secret.refresh_token.is_none() {
            secret.refresh_token = fallback_refresh_token;
        }
        record_oauth_recovery(
            CodexOAuthRecoveryOperation::AuthorizationCode,
            pending
                .reauthorization()
                .map(|account_id| account_id.as_str()),
            secret.access_token.expose_secret(),
            secret
                .refresh_token
                .as_ref()
                .map(ExposeSecret::expose_secret),
        );
        Ok((secret, tokens.id_token, tokens.expires_in))
    }
}

fn current_oauth_refresh_token(
    current: &LoadedCredential,
) -> Result<Option<SecretString>, CodexOAuthAdminError> {
    let runtime = CodexCredentialCodec::decode(&current.credential)
        .map_err(|_| CodexOAuthAdminError::Credential)?;
    Ok(runtime
        .authentication
        .oauth()
        .and_then(|oauth| oauth.refresh_token.clone()))
}

fn authorization_url(
    pending: &CodexPendingAuthorization,
    oauth_client_id: &str,
) -> Result<String, CodexOAuthAdminError> {
    let mut url =
        Url::parse(AUTHORIZATION_ENDPOINT).map_err(|_| CodexOAuthAdminError::InvalidInput)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(
        pending.code_verifier.expose_secret().as_bytes(),
    ));
    let query = [
        ("response_type", "code"),
        ("client_id", oauth_client_id),
        ("redirect_uri", OFFICIAL_CODEX_REDIRECT_URI),
        ("scope", AUTHORIZATION_SCOPE),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("state", pending.state.expose_secret()),
        ("originator", AUTHORIZATION_ORIGINATOR),
    ]
    .map(|(name, value)| format!("{name}={}", urlencoding::encode(value)))
    .join("&");
    url.set_query(Some(&query));
    Ok(url.into())
}

fn callback_parts(value: &str) -> Result<(SecretString, SecretString), CodexOAuthAdminError> {
    let url = Url::parse(value).map_err(|_| CodexOAuthAdminError::CallbackRejected)?;
    let mut code = None;
    let mut state = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => set_unique_callback_parameter(&mut code, value.into_owned())?,
            "state" => set_unique_callback_parameter(&mut state, value.into_owned())?,
            // 回调地址只承载 query 参数；安全绑定由唯一的 code/state、
            // 服务端保存的 state 和 PKCE token exchange 共同完成。
            _ => {}
        }
    }
    let code = code.filter(|value| !value.is_empty());
    let state = state.filter(|value| !value.is_empty());
    match (code, state) {
        (Some(code), Some(state)) => Ok((SecretString::from(code), SecretString::from(state))),
        _ => Err(CodexOAuthAdminError::CallbackRejected),
    }
}

fn set_unique_callback_parameter(
    target: &mut Option<String>,
    value: String,
) -> Result<(), CodexOAuthAdminError> {
    if target.replace(value).is_some() {
        return Err(CodexOAuthAdminError::CallbackRejected);
    }
    Ok(())
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
        (AuthorizationMutationTarget::Reauthorize { account_id }, Some(target)) => {
            account_id == target
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

fn map_authorization_settlement_error(error: CodexOAuthPendingStoreError) -> AdminError {
    match error {
        CodexOAuthPendingStoreError::InvalidValue => {
            AdminError::internal("OpenAI OAuth pending claim is invalid")
        }
        CodexOAuthPendingStoreError::Conflict => {
            AdminError::conflict("OpenAI OAuth pending claim conflicts with current state")
        }
        CodexOAuthPendingStoreError::Unavailable => {
            AdminError::unavailable("OpenAI OAuth pending claim store is unavailable")
        }
    }
}

fn map_exchange_error(error: AuthorizationCodeExchangeError) -> CodexOAuthAdminError {
    match error {
        AuthorizationCodeExchangeError::Rejected => CodexOAuthAdminError::TokenRejected,
        AuthorizationCodeExchangeError::Unavailable => CodexOAuthAdminError::UpstreamUnavailable,
        AuthorizationCodeExchangeError::Ambiguous => CodexOAuthAdminError::Ambiguous,
    }
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
