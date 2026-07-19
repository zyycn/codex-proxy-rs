//! Codex 明文 credential JSON 的 schema 校验与日志脱敏边界。

use gateway_core::engine::credential::PlaintextCredential;
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use thiserror::Error;

use super::types::{CodexCookie, CodexCredentialData, CodexOAuthSecret, RuntimeCodexCookie};

const CODEX_CREDENTIAL_SCHEMA_VERSION: u32 = 1;
const MAX_CREDENTIAL_BYTES: usize = 256 * 1024;
const MAX_TOKEN_BYTES: usize = 128 * 1024;
const MAX_COOKIES: usize = 128;

/// 已解析且只在 Provider 内可见的认证材料。
pub struct CodexRuntimeCredential {
    pub secret: CodexOAuthSecret,
    pub cookies: Vec<RuntimeCodexCookie>,
    pub oauth_client_id: Option<String>,
    pub oauth_scope: Option<String>,
}

impl std::fmt::Debug for CodexRuntimeCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexRuntimeCredential")
            .field("secret", &self.secret)
            .field("cookies", &self.cookies)
            .field("oauth_client_id", &self.oauth_client_id)
            .field("oauth_scope", &self.oauth_scope)
            .finish()
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
    pub fn encode(
        secret: &CodexOAuthSecret,
        cookies: Vec<CodexCookie>,
    ) -> Result<PlaintextCredential, CodexCredentialDataError> {
        Self::encode_complete(CodexCredentialData {
            schema_version: CODEX_CREDENTIAL_SCHEMA_VERSION,
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
        })
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
        Ok(CodexRuntimeCredential {
            secret: CodexOAuthSecret {
                access_token: SecretString::from(data.access_token),
                refresh_token: data.refresh_token.map(SecretString::from),
                id_token: data.id_token.map(SecretString::from),
            },
            cookies: data
                .cookies
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
            oauth_client_id: data.oauth_client_id,
            oauth_scope: data.oauth_scope,
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
}

fn validate(data: &CodexCredentialData) -> Result<(), CodexCredentialDataError> {
    if data.schema_version != CODEX_CREDENTIAL_SCHEMA_VERSION
        || !valid_secret(&data.access_token)
        || data
            .refresh_token
            .as_deref()
            .is_some_and(|value| !valid_secret(value))
        || data
            .id_token
            .as_deref()
            .is_some_and(|value| !valid_secret(value))
        || data.cookies.len() > MAX_COOKIES
        || data.cookies.iter().any(|cookie| {
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

fn valid_secret(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_TOKEN_BYTES && !value.chars().any(char::is_control)
}
