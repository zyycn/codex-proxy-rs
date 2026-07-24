//! Codex 明文 credential JSON 的 schema 校验与日志脱敏边界。

use gateway_core::engine::credential::PlaintextCredential;
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use thiserror::Error;

use super::agent_identity::{CodexAgentIdentityError, CodexAgentIdentitySecret};
use super::types::{
    CodexAccountProfile, CodexAgentIdentityCredentialData, CodexCookie, CodexCredentialData,
    CodexCredentialPrincipal, CodexOAuthCredentialData, CodexOAuthSecret, RuntimeCodexCookie,
};

const CODEX_CREDENTIAL_SCHEMA_VERSION: u32 = 1;
const MAX_CREDENTIAL_BYTES: usize = 256 * 1024;
const MAX_TOKEN_BYTES: usize = 128 * 1024;
const MAX_COOKIES: usize = 128;

/// 已解析且只在 Provider 内可见的认证材料。
pub struct CodexRuntimeCredential {
    pub authentication: CodexRuntimeAuthentication,
    pub principal: Option<CodexCredentialPrincipal>,
    pub installation_id: String,
    pub cookies: Vec<RuntimeCodexCookie>,
    pub oauth_client_id: Option<String>,
    pub oauth_scope: Option<String>,
}

impl std::fmt::Debug for CodexRuntimeCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexRuntimeCredential")
            .field("authentication", &self.authentication)
            .field("principal", &self.principal)
            .field("installation_id", &"<pseudonymous>")
            .field("cookies", &self.cookies)
            .field("oauth_client_id", &self.oauth_client_id)
            .field("oauth_scope", &self.oauth_scope)
            .finish()
    }
}

pub enum CodexRuntimeAuthentication {
    OAuth(CodexOAuthSecret),
    AgentIdentity(Box<CodexAgentIdentitySecret>),
}

impl CodexRuntimeAuthentication {
    #[must_use]
    pub const fn is_agent_identity(&self) -> bool {
        matches!(self, Self::AgentIdentity(_))
    }

    pub fn authorization_header(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<SecretString, CodexCredentialDataError> {
        match self {
            Self::OAuth(secret) => Ok(SecretString::from(format!(
                "Bearer {}",
                secret.access_token.expose_secret()
            ))),
            Self::AgentIdentity(secret) => secret
                .authorization_header(now)
                .map_err(|_| CodexCredentialDataError::Invalid),
        }
    }

    #[must_use]
    pub const fn oauth(&self) -> Option<&CodexOAuthSecret> {
        match self {
            Self::OAuth(secret) => Some(secret),
            Self::AgentIdentity(_) => None,
        }
    }
}

impl std::fmt::Debug for CodexRuntimeAuthentication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OAuth(secret) => secret.fmt(formatter),
            Self::AgentIdentity(secret) => secret.fmt(formatter),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CodexCredentialDataError {
    #[error("Codex credential JSON is invalid")]
    Invalid,
    #[error("Codex credential JSON exceeds its size limit")]
    TooLarge,
}

/// 不加密、不解密；只验证 Codex-owned JSON 并做运行时 secret 包装。
#[derive(Debug, Default, Clone, Copy)]
pub struct CodexCredentialCodec;

impl CodexCredentialCodec {
    pub fn encode_new(
        secret: &CodexOAuthSecret,
        account: &CodexAccountProfile,
        cookies: Vec<CodexCookie>,
    ) -> Result<PlaintextCredential, CodexCredentialDataError> {
        Self::encode_complete(CodexCredentialData::OAuth(CodexOAuthCredentialData {
            schema_version: CODEX_CREDENTIAL_SCHEMA_VERSION,
            principal: CodexCredentialPrincipal {
                oauth_subject: account.oauth_subject.clone(),
                poid: account.poid.clone(),
            },
            installation_id: uuid::Uuid::new_v4().to_string(),
            access_token: secret.access_token.expose_secret().to_owned(),
            refresh_token: secret
                .refresh_token
                .as_ref()
                .map(|value| value.expose_secret().to_owned()),
            id_token: secret
                .id_token
                .as_ref()
                .map(|value| value.expose_secret().to_owned()),
            oauth_client_id: None,
            oauth_scope: None,
            cookies,
        }))
    }

    pub fn encode_agent_identity(
        data: CodexAgentIdentityCredentialData,
    ) -> Result<PlaintextCredential, CodexCredentialDataError> {
        Self::encode_complete(CodexCredentialData::AgentIdentity(data))
    }

    pub fn encode_complete(
        data: CodexCredentialData,
    ) -> Result<PlaintextCredential, CodexCredentialDataError> {
        validate(&data)?;
        let value = serde_json::to_value(data).map_err(|_| CodexCredentialDataError::Invalid)?;
        let object = value
            .as_object()
            .cloned()
            .ok_or(CodexCredentialDataError::Invalid)?;
        if serde_json::to_vec(&object)
            .map_err(|_| CodexCredentialDataError::Invalid)?
            .len()
            > MAX_CREDENTIAL_BYTES
        {
            return Err(CodexCredentialDataError::TooLarge);
        }
        Ok(PlaintextCredential::new(object))
    }

    pub fn decode(
        credential: &PlaintextCredential,
    ) -> Result<CodexRuntimeCredential, CodexCredentialDataError> {
        let value = Value::Object(credential.expose_to_provider().clone());
        if serde_json::to_vec(&value)
            .map_err(|_| CodexCredentialDataError::Invalid)?
            .len()
            > MAX_CREDENTIAL_BYTES
        {
            return Err(CodexCredentialDataError::TooLarge);
        }
        let data = serde_json::from_value::<CodexCredentialData>(value)
            .map_err(|_| CodexCredentialDataError::Invalid)?;
        validate(&data)?;
        let (authentication, principal, installation_id, cookies, oauth_client_id, oauth_scope) =
            match data {
                CodexCredentialData::OAuth(data) => (
                    CodexRuntimeAuthentication::OAuth(CodexOAuthSecret {
                        access_token: SecretString::from(data.access_token),
                        refresh_token: data.refresh_token.map(SecretString::from),
                        id_token: data.id_token.map(SecretString::from),
                    }),
                    Some(data.principal),
                    data.installation_id,
                    data.cookies,
                    data.oauth_client_id,
                    data.oauth_scope,
                ),
                CodexCredentialData::AgentIdentity(data) => (
                    CodexRuntimeAuthentication::AgentIdentity(Box::new(
                        CodexAgentIdentitySecret::from_pkcs8(
                            data.agent_runtime_id,
                            &data.agent_private_key,
                            data.task_id,
                        )
                        .map_err(map_agent_identity_error)?,
                    )),
                    None,
                    data.installation_id,
                    data.cookies,
                    None,
                    None,
                ),
            };
        Ok(CodexRuntimeCredential {
            authentication,
            principal,
            installation_id,
            cookies: cookies
                .into_iter()
                .map(|cookie| RuntimeCodexCookie {
                    name: cookie.name,
                    value: SecretString::from(cookie.value),
                    domain: cookie.domain,
                    path: cookie.path,
                    host_only: cookie.host_only,
                    secure: cookie.secure,
                    expires_at: cookie.expires_at,
                })
                .collect(),
            oauth_client_id,
            oauth_scope,
        })
    }

    pub fn decode_complete(
        credential: &PlaintextCredential,
    ) -> Result<CodexCredentialData, CodexCredentialDataError> {
        let value = Value::Object(credential.expose_to_provider().clone());
        let data = serde_json::from_value::<CodexCredentialData>(value)
            .map_err(|_| CodexCredentialDataError::Invalid)?;
        validate(&data)?;
        Ok(data)
    }

    pub fn preserve_installation_id(
        incoming: &PlaintextCredential,
        existing: &PlaintextCredential,
    ) -> Result<PlaintextCredential, CodexCredentialDataError> {
        let mut incoming = Self::decode_complete(incoming)?;
        let existing = Self::decode_complete(existing)?;
        match (&mut incoming, existing) {
            (CodexCredentialData::OAuth(incoming), CodexCredentialData::OAuth(existing)) => {
                if incoming.principal != existing.principal {
                    return Err(CodexCredentialDataError::Invalid);
                }
                incoming.installation_id = existing.installation_id;
            }
            (
                CodexCredentialData::AgentIdentity(incoming),
                CodexCredentialData::AgentIdentity(existing),
            ) => {
                if incoming.agent_runtime_id != existing.agent_runtime_id {
                    return Err(CodexCredentialDataError::Invalid);
                }
                incoming.installation_id = existing.installation_id;
                if incoming.task_id.is_none()
                    && incoming.agent_private_key == existing.agent_private_key
                {
                    incoming.task_id = existing.task_id;
                }
            }
            _ => return Err(CodexCredentialDataError::Invalid),
        }
        Self::encode_complete(incoming)
    }
}

fn validate(data: &CodexCredentialData) -> Result<(), CodexCredentialDataError> {
    let (installation_id, cookies) = match data {
        CodexCredentialData::OAuth(data) => {
            if data.schema_version != CODEX_CREDENTIAL_SCHEMA_VERSION
                || !valid_identity(&data.principal.oauth_subject)
                || data
                    .principal
                    .poid
                    .as_deref()
                    .is_some_and(|value| !valid_identity(value))
                || !valid_secret(&data.access_token)
                || data
                    .refresh_token
                    .as_deref()
                    .is_some_and(|value| !valid_secret(value))
                || data
                    .id_token
                    .as_deref()
                    .is_some_and(|value| !valid_secret(value))
            {
                return Err(CodexCredentialDataError::Invalid);
            }
            (&data.installation_id, &data.cookies)
        }
        CodexCredentialData::AgentIdentity(data) => {
            if data.schema_version != CODEX_CREDENTIAL_SCHEMA_VERSION
                || !valid_identity(&data.agent_runtime_id)
                || !valid_secret(&data.agent_private_key)
                || data
                    .task_id
                    .as_deref()
                    .is_some_and(|value| !valid_identity(value))
                || CodexAgentIdentitySecret::from_pkcs8(
                    data.agent_runtime_id.clone(),
                    &data.agent_private_key,
                    data.task_id.clone(),
                )
                .is_err()
            {
                return Err(CodexCredentialDataError::Invalid);
            }
            (&data.installation_id, &data.cookies)
        }
    };
    if !valid_installation_id(installation_id)
        || cookies.len() > MAX_COOKIES
        || cookies.iter().any(|cookie| {
            cookie.name.is_empty()
                || cookie.name.len() > 256
                || cookie.value.is_empty()
                || cookie.value.len() > 16 * 1024
                || cookie.name.chars().any(char::is_control)
                || cookie.value.chars().any(char::is_control)
                || cookie.domain.chars().any(char::is_control)
                || cookie.path.chars().any(char::is_control)
        })
    {
        return Err(CodexCredentialDataError::Invalid);
    }
    Ok(())
}

const fn map_agent_identity_error(_: CodexAgentIdentityError) -> CodexCredentialDataError {
    CodexCredentialDataError::Invalid
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 2_048 && !value.chars().any(char::is_control)
}

fn valid_installation_id(value: &str) -> bool {
    uuid::Uuid::parse_str(value)
        .ok()
        .is_some_and(|uuid| uuid.get_version_num() == 4)
}

fn valid_secret(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_TOKEN_BYTES && !value.chars().any(char::is_control)
}
