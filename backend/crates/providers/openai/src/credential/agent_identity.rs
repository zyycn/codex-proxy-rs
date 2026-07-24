//! OpenAI Agent Identity 签名、task 注册与 revision-fenced 恢复。

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::{DateTime, SecondsFormat, Utc};
use crypto_box::SecretKey as BoxSecretKey;
use ed25519_dalek::pkcs8::DecodePrivateKey as _;
use ed25519_dalek::{Signer as _, SigningKey};
use gateway_core::engine::credential::{ProviderAccount, ProviderAccountId};
use reqwest::StatusCode;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha512};
use thiserror::Error;
use tokio::sync::Mutex;
use url::Url;

use super::repository::{CodexCredentialRepository, CredentialRepositoryError};
use super::security::{CodexRuntimeAuthentication, CodexRuntimeCredential};
use crate::transport::websocket::CodexWebSocketExchangeError;
use crate::transport::{CodexClientError, CodexWebSocketPool};

const OFFICIAL_AGENT_IDENTITY_AUTH_BASE_URL: &str = "https://auth.openai.com/api/accounts/";
const MAX_TASK_RESPONSE_BYTES: usize = 64 * 1024;

/// 已验证且只存在于 OpenAI Provider 运行时的 Agent Identity 密钥。
pub struct CodexAgentIdentitySecret {
    runtime_id: String,
    signing_key: SigningKey,
    task_id: Option<String>,
}

impl CodexAgentIdentitySecret {
    pub fn from_pkcs8(
        runtime_id: String,
        encoded_private_key: &str,
        task_id: Option<String>,
    ) -> Result<Self, CodexAgentIdentityError> {
        if !valid_identifier(&runtime_id)
            || task_id
                .as_deref()
                .is_some_and(|value| !valid_identifier(value))
        {
            return Err(CodexAgentIdentityError::InvalidCredential);
        }
        let der = STANDARD
            .decode(encoded_private_key.trim())
            .map_err(|_| CodexAgentIdentityError::InvalidCredential)?;
        let signing_key = SigningKey::from_pkcs8_der(&der)
            .map_err(|_| CodexAgentIdentityError::InvalidCredential)?;
        Ok(Self {
            runtime_id,
            signing_key,
            task_id,
        })
    }

    #[must_use]
    pub fn runtime_id(&self) -> &str {
        &self.runtime_id
    }

    #[must_use]
    pub fn task_id(&self) -> Option<&str> {
        self.task_id.as_deref()
    }

    pub fn authorization_header(
        &self,
        now: DateTime<Utc>,
    ) -> Result<SecretString, CodexAgentIdentityError> {
        let task_id = self.task_id().ok_or(CodexAgentIdentityError::MissingTask)?;
        let timestamp = timestamp(now);
        let payload = format!("{}:{task_id}:{timestamp}", self.runtime_id);
        let signature = self.signing_key.sign(payload.as_bytes());
        let envelope = AgentAssertionEnvelope {
            agent_runtime_id: &self.runtime_id,
            task_id,
            timestamp: &timestamp,
            signature: STANDARD.encode(signature.to_bytes()),
        };
        let encoded = serde_json::to_vec(&envelope)
            .map_err(|_| CodexAgentIdentityError::InvalidCredential)?;
        Ok(SecretString::from(format!(
            "AgentAssertion {}",
            URL_SAFE_NO_PAD.encode(encoded)
        )))
    }

    fn registration(&self, now: DateTime<Utc>) -> AgentTaskRegistration {
        let timestamp = timestamp(now);
        let payload = format!("{}:{timestamp}", self.runtime_id);
        let signature = self.signing_key.sign(payload.as_bytes());
        AgentTaskRegistration {
            timestamp,
            signature: STANDARD.encode(signature.to_bytes()),
        }
    }

    fn decrypt_task_id(&self, encoded: &str) -> Result<String, CodexAgentIdentityError> {
        let ciphertext = STANDARD
            .decode(encoded.trim())
            .map_err(|_| CodexAgentIdentityError::InvalidTaskResponse)?;
        let digest = Sha512::digest(self.signing_key.to_bytes());
        let mut curve_key = [0_u8; 32];
        curve_key.copy_from_slice(&digest[..32]);
        let secret = BoxSecretKey::from(curve_key);
        let plaintext = secret
            .unseal(&ciphertext)
            .map_err(|_| CodexAgentIdentityError::InvalidTaskResponse)?;
        let task_id = String::from_utf8(plaintext)
            .map_err(|_| CodexAgentIdentityError::InvalidTaskResponse)?;
        let task_id = task_id.trim().to_owned();
        if !valid_identifier(&task_id) {
            return Err(CodexAgentIdentityError::InvalidTaskResponse);
        }
        Ok(task_id)
    }
}

impl fmt::Debug for CodexAgentIdentitySecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexAgentIdentitySecret")
            .field("runtime_id", &"<redacted>")
            .field("signing_key", &"<redacted>")
            .field("task_id", &self.task_id.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Serialize)]
struct AgentAssertionEnvelope<'a> {
    agent_runtime_id: &'a str,
    task_id: &'a str,
    timestamp: &'a str,
    signature: String,
}

#[derive(Serialize)]
struct AgentTaskRegistration {
    timestamp: String,
    signature: String,
}

#[derive(Deserialize)]
struct AgentTaskRegistrationResponse {
    #[serde(default, alias = "taskId")]
    task_id: Option<String>,
    #[serde(default, alias = "encryptedTaskId")]
    encrypted_task_id: Option<String>,
}

#[async_trait]
pub trait CodexAgentIdentityTaskRegistrar: Send + Sync + 'static {
    async fn register(
        &self,
        credential: &CodexAgentIdentitySecret,
    ) -> Result<String, CodexAgentIdentityError>;
}

pub struct OfficialCodexAgentIdentityTaskRegistrar {
    client: reqwest::Client,
    base_url: Url,
}

impl OfficialCodexAgentIdentityTaskRegistrar {
    pub fn new(client: reqwest::Client) -> Result<Self, CodexAgentIdentityError> {
        let base_url = Url::parse(OFFICIAL_AGENT_IDENTITY_AUTH_BASE_URL)
            .map_err(|_| CodexAgentIdentityError::InvalidConfiguration)?;
        Ok(Self { client, base_url })
    }
}

impl fmt::Debug for OfficialCodexAgentIdentityTaskRegistrar {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OfficialCodexAgentIdentityTaskRegistrar")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl CodexAgentIdentityTaskRegistrar for OfficialCodexAgentIdentityTaskRegistrar {
    async fn register(
        &self,
        credential: &CodexAgentIdentitySecret,
    ) -> Result<String, CodexAgentIdentityError> {
        let mut endpoint = self.base_url.clone();
        endpoint
            .path_segments_mut()
            .map_err(|_| CodexAgentIdentityError::InvalidConfiguration)?
            .extend(["v1", "agent", credential.runtime_id(), "task", "register"]);
        let response = self
            .client
            .post(endpoint)
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&credential.registration(Utc::now()))
            .send()
            .await
            .map_err(|_| CodexAgentIdentityError::TaskRegistrationUnavailable)?;
        if !response.status().is_success() {
            return Err(CodexAgentIdentityError::TaskRegistrationRejected);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_TASK_RESPONSE_BYTES as u64)
        {
            return Err(CodexAgentIdentityError::InvalidTaskResponse);
        }
        let body = response
            .bytes()
            .await
            .map_err(|_| CodexAgentIdentityError::TaskRegistrationUnavailable)?;
        if body.len() > MAX_TASK_RESPONSE_BYTES {
            return Err(CodexAgentIdentityError::InvalidTaskResponse);
        }
        let result = serde_json::from_slice::<AgentTaskRegistrationResponse>(&body)
            .map_err(|_| CodexAgentIdentityError::InvalidTaskResponse)?;
        if let Some(task_id) = result.task_id.map(|value| value.trim().to_owned())
            && valid_identifier(&task_id)
        {
            return Ok(task_id);
        }
        let encrypted = result
            .encrypted_task_id
            .as_deref()
            .ok_or(CodexAgentIdentityError::InvalidTaskResponse)?;
        credential.decrypt_task_id(encrypted)
    }
}

pub struct PreparedCodexRuntimeCredential {
    pub account: ProviderAccount,
    pub credential: CodexRuntimeCredential,
}

/// Agent task 的唯一注册与恢复 owner。
pub struct CodexAgentIdentityTaskService {
    repository: CodexCredentialRepository,
    registrar: Arc<dyn CodexAgentIdentityTaskRegistrar>,
    websocket_pool: Arc<CodexWebSocketPool>,
    account_locks: Mutex<HashMap<ProviderAccountId, Arc<Mutex<()>>>>,
}

impl CodexAgentIdentityTaskService {
    #[must_use]
    pub fn new(
        repository: CodexCredentialRepository,
        registrar: Arc<dyn CodexAgentIdentityTaskRegistrar>,
        websocket_pool: Arc<CodexWebSocketPool>,
    ) -> Self {
        Self {
            repository,
            registrar,
            websocket_pool,
            account_locks: Mutex::new(HashMap::new()),
        }
    }

    pub async fn prepare(
        &self,
        account: &ProviderAccount,
    ) -> Result<PreparedCodexRuntimeCredential, CodexAgentIdentityError> {
        let prepared = self.load_current(account.id()).await?;
        if !matches!(
            &prepared.credential.authentication,
            CodexRuntimeAuthentication::AgentIdentity(secret) if secret.task_id().is_none()
        ) {
            return Ok(prepared);
        }
        self.ensure_task(account.id(), None).await
    }

    pub async fn recover(
        &self,
        account_id: &ProviderAccountId,
        expected_task_id: &str,
    ) -> Result<PreparedCodexRuntimeCredential, CodexAgentIdentityError> {
        self.ensure_task(account_id, Some(expected_task_id)).await
    }

    pub async fn recover_after_rejected_task(
        &self,
        account_id: &ProviderAccountId,
        authentication: &CodexRuntimeAuthentication,
        error: &CodexClientError,
    ) -> Result<Option<PreparedCodexRuntimeCredential>, CodexAgentIdentityError> {
        let CodexRuntimeAuthentication::AgentIdentity(secret) = authentication else {
            return Ok(None);
        };
        let Some(task_id) = secret.task_id() else {
            return Ok(None);
        };
        if !is_agent_identity_task_invalid_client_error(error) {
            return Ok(None);
        }
        self.recover(account_id, task_id).await.map(Some)
    }

    async fn ensure_task(
        &self,
        account_id: &ProviderAccountId,
        expected_task_id: Option<&str>,
    ) -> Result<PreparedCodexRuntimeCredential, CodexAgentIdentityError> {
        let lock = {
            let mut locks = self.account_locks.lock().await;
            Arc::clone(
                locks
                    .entry(account_id.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        let _guard = lock.lock().await;
        let current = self.load_current(account_id).await?;
        let CodexRuntimeAuthentication::AgentIdentity(secret) = &current.credential.authentication
        else {
            return Ok(current);
        };
        match (expected_task_id, secret.task_id()) {
            (None, Some(_)) => return Ok(current),
            (Some(expected), Some(actual)) if actual != expected => return Ok(current),
            _ => {}
        }
        let task_id = self.registrar.register(secret).await?;
        let mut data = self.repository.load_complete_data(&current.account).await?;
        let agent = data
            .agent_identity_mut()
            .ok_or(CodexAgentIdentityError::InvalidCredential)?;
        match (expected_task_id, agent.task_id.as_deref()) {
            (None, Some(_)) => return self.load_current(account_id).await,
            (Some(expected), Some(actual)) if actual != expected => {
                return self.load_current(account_id).await;
            }
            _ => agent.task_id = Some(task_id),
        }
        self.repository
            .compare_and_swap_data(&current.account, data)
            .await?;
        self.websocket_pool.evict_account(account_id.as_str()).await;
        self.load_current(account_id).await
    }

    async fn load_current(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<PreparedCodexRuntimeCredential, CodexAgentIdentityError> {
        let account = self
            .repository
            .store()
            .get_account(account_id)
            .await?
            .filter(|account| account.provider().as_str() == "openai")
            .ok_or(CodexAgentIdentityError::NotFound)?;
        let credential = self.repository.load_runtime_credential(&account).await?;
        Ok(PreparedCodexRuntimeCredential {
            account,
            credential,
        })
    }
}

impl fmt::Debug for CodexAgentIdentityTaskService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexAgentIdentityTaskService")
            .field("repository", &"CodexCredentialRepository")
            .field("registrar", &"CodexAgentIdentityTaskRegistrar")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error)]
pub enum CodexAgentIdentityError {
    #[error("OpenAI Agent Identity credential is invalid")]
    InvalidCredential,
    #[error("OpenAI Agent Identity task is unavailable")]
    MissingTask,
    #[error("OpenAI Agent Identity task registration is unavailable")]
    TaskRegistrationUnavailable,
    #[error("OpenAI Agent Identity task registration was rejected")]
    TaskRegistrationRejected,
    #[error("OpenAI Agent Identity task registration response is invalid")]
    InvalidTaskResponse,
    #[error("OpenAI Agent Identity configuration is invalid")]
    InvalidConfiguration,
    #[error("OpenAI Agent Identity account was not found")]
    NotFound,
    #[error("OpenAI Agent Identity credential store is unavailable")]
    Repository,
}

impl From<CredentialRepositoryError> for CodexAgentIdentityError {
    fn from(_: CredentialRepositoryError) -> Self {
        Self::Repository
    }
}

impl From<gateway_core::error::StoreError> for CodexAgentIdentityError {
    fn from(_: gateway_core::error::StoreError) -> Self {
        Self::Repository
    }
}

#[must_use]
pub fn is_agent_identity_task_invalid_response(status: StatusCode, body: &str) -> bool {
    if status != StatusCode::UNAUTHORIZED {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    let compact = lower
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect::<String>();
    [
        "\"code\":\"invalid_task_id\"",
        "\"code\":\"task_not_found\"",
        "\"code\":\"task_expired\"",
        "\"error\":\"invalid_task_id\"",
    ]
    .iter()
    .any(|marker| compact.contains(marker))
        || [
            "invalid task_id",
            "invalid task id",
            "task_id is invalid",
            "task id is invalid",
            "task not found",
            "task expired",
            "unknown task_id",
            "unknown task id",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
}

fn is_agent_identity_task_invalid_client_error(error: &CodexClientError) -> bool {
    match error {
        CodexClientError::Upstream { status, body, .. } => {
            is_agent_identity_task_invalid_response(*status, body)
        }
        CodexClientError::WebSocket(CodexWebSocketExchangeError::Upstream(upstream)) => {
            StatusCode::from_u16(upstream.status_code)
                .ok()
                .is_some_and(|status| {
                    is_agent_identity_task_invalid_response(status, &upstream.body)
                })
        }
        CodexClientError::WebSocket(CodexWebSocketExchangeError::PostSendAmbiguous {
            source: Some(source),
            ..
        }) => is_agent_identity_task_invalid_websocket_error(source),
        _ => false,
    }
}

fn is_agent_identity_task_invalid_websocket_error(error: &CodexWebSocketExchangeError) -> bool {
    match error {
        CodexWebSocketExchangeError::Upstream(upstream) => {
            StatusCode::from_u16(upstream.status_code)
                .ok()
                .is_some_and(|status| {
                    is_agent_identity_task_invalid_response(status, &upstream.body)
                })
        }
        CodexWebSocketExchangeError::PostSendAmbiguous {
            source: Some(source),
            ..
        } => is_agent_identity_task_invalid_websocket_error(source),
        _ => false,
    }
}

fn timestamp(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 2_048 && !value.chars().any(char::is_control)
}
