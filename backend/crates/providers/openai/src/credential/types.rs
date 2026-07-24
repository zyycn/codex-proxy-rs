//! Codex credential 的 Provider-owned 明文结构与安全运行时值对象。

use std::fmt;

use chrono::{DateTime, Utc};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use url::Url;

/// OAuth AT/RT/ID Token；`Debug` 永不输出明文。
#[derive(Clone)]
pub struct CodexOAuthSecret {
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,
    pub id_token: Option<SecretString>,
}

impl fmt::Debug for CodexOAuthSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexOAuthSecret")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("id_token", &self.id_token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// 已由官方 JWT/OIDC 与认证 usage 响应共同确认的完整账号投影。
#[derive(Clone)]
pub struct CodexAccountProfile {
    pub email: Option<String>,
    pub oauth_subject: String,
    pub poid: Option<String>,
    pub chatgpt_account_id: String,
    pub chatgpt_user_id: String,
    pub plan_type: Option<String>,
    pub access_token_expires_at: Option<DateTime<Utc>>,
}

impl fmt::Debug for CodexAccountProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexAccountProfile")
            .field("email", &self.email.as_ref().map(|_| "<redacted>"))
            .field("oauth_subject", &"<redacted>")
            .field("poid", &self.poid.as_ref().map(|_| "<redacted>"))
            .field("chatgpt_account_id", &"<redacted>")
            .field("chatgpt_user_id", &"<redacted>")
            .field("plan_type", &self.plan_type)
            .field("access_token_expires_at", &self.access_token_expires_at)
            .finish()
    }
}

/// 持久化在 Provider credential JSON 中的签名认证主体。
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodexCredentialPrincipal {
    pub oauth_subject: String,
    pub poid: Option<String>,
}

impl fmt::Debug for CodexCredentialPrincipal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCredentialPrincipal")
            .field("oauth_subject", &"<redacted>")
            .field("poid", &self.poid.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// 存在 `provider_credentials_json` 内的 Cookie。
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub host_only: bool,
    pub secure: bool,
    pub expires_at: Option<DateTime<Utc>>,
}

impl fmt::Debug for CodexCookie {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCookie")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .field("domain", &self.domain)
            .field("path", &self.path)
            .field("host_only", &self.host_only)
            .field("secure", &self.secure)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

pub const CODEX_AUTHENTICATION_KIND_OAUTH: &str = "oauth";
pub const CODEX_AUTHENTICATION_KIND_AGENT_IDENTITY: &str = "agent_identity";

/// Codex OAuth 对 `provider_credentials_json` 的完整明文 schema。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexOAuthCredentialData {
    pub schema_version: u32,
    pub principal: CodexCredentialPrincipal,
    pub installation_id: String,
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_scope: Option<String>,
    #[serde(default)]
    pub cookies: Vec<CodexCookie>,
}

impl fmt::Debug for CodexOAuthCredentialData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexOAuthCredentialData")
            .field("schema_version", &self.schema_version)
            .field("principal", &self.principal)
            .field("installation_id", &"<pseudonymous>")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("id_token", &self.id_token.as_ref().map(|_| "<redacted>"))
            .field("oauth_client_id", &self.oauth_client_id)
            .field("oauth_scope", &self.oauth_scope)
            .field("cookies", &self.cookies)
            .finish()
    }
}

/// Agent Identity 文档的固定认证模式。
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum CodexAgentIdentityAuthMode {
    #[serde(rename = "agentIdentity")]
    AgentIdentity,
}

/// OpenAI Agent Identity 对 `provider_credentials_json` 的完整明文 schema。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexAgentIdentityCredentialData {
    pub schema_version: u32,
    pub auth_mode: CodexAgentIdentityAuthMode,
    pub installation_id: String,
    pub agent_runtime_id: String,
    pub agent_private_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default)]
    pub cookies: Vec<CodexCookie>,
}

impl fmt::Debug for CodexAgentIdentityCredentialData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexAgentIdentityCredentialData")
            .field("schema_version", &self.schema_version)
            .field("auth_mode", &self.auth_mode)
            .field("installation_id", &"<pseudonymous>")
            .field("agent_runtime_id", &"<redacted>")
            .field("agent_private_key", &"<redacted>")
            .field("task_id", &self.task_id.as_ref().map(|_| "<redacted>"))
            .field("cookies", &self.cookies)
            .finish()
    }
}

/// OpenAI Provider 支持的两种规范化凭据形态。
#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CodexCredentialData {
    OAuth(CodexOAuthCredentialData),
    AgentIdentity(CodexAgentIdentityCredentialData),
}

impl CodexCredentialData {
    #[must_use]
    pub const fn authentication_kind(&self) -> &'static str {
        match self {
            Self::OAuth(_) => CODEX_AUTHENTICATION_KIND_OAUTH,
            Self::AgentIdentity(_) => CODEX_AUTHENTICATION_KIND_AGENT_IDENTITY,
        }
    }

    #[must_use]
    pub fn installation_id(&self) -> &str {
        match self {
            Self::OAuth(data) => &data.installation_id,
            Self::AgentIdentity(data) => &data.installation_id,
        }
    }

    pub fn set_installation_id(&mut self, installation_id: String) {
        match self {
            Self::OAuth(data) => data.installation_id = installation_id,
            Self::AgentIdentity(data) => data.installation_id = installation_id,
        }
    }

    #[must_use]
    pub fn cookies(&self) -> &[CodexCookie] {
        match self {
            Self::OAuth(data) => &data.cookies,
            Self::AgentIdentity(data) => &data.cookies,
        }
    }

    pub fn cookies_mut(&mut self) -> &mut Vec<CodexCookie> {
        match self {
            Self::OAuth(data) => &mut data.cookies,
            Self::AgentIdentity(data) => &mut data.cookies,
        }
    }

    #[must_use]
    pub fn oauth(&self) -> Option<&CodexOAuthCredentialData> {
        match self {
            Self::OAuth(data) => Some(data),
            Self::AgentIdentity(_) => None,
        }
    }

    pub fn oauth_mut(&mut self) -> Option<&mut CodexOAuthCredentialData> {
        match self {
            Self::OAuth(data) => Some(data),
            Self::AgentIdentity(_) => None,
        }
    }

    #[must_use]
    pub fn agent_identity(&self) -> Option<&CodexAgentIdentityCredentialData> {
        match self {
            Self::OAuth(_) => None,
            Self::AgentIdentity(data) => Some(data),
        }
    }

    pub fn agent_identity_mut(&mut self) -> Option<&mut CodexAgentIdentityCredentialData> {
        match self {
            Self::OAuth(_) => None,
            Self::AgentIdentity(data) => Some(data),
        }
    }

    #[must_use]
    pub fn has_refresh_token(&self) -> bool {
        self.oauth()
            .is_some_and(|data| data.refresh_token.is_some())
    }
}

impl fmt::Debug for CodexCredentialData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OAuth(data) => data.fmt(formatter),
            Self::AgentIdentity(data) => data.fmt(formatter),
        }
    }
}

/// 刷新成功后的 CAS 输入。
pub struct RotateCodexCredential {
    pub account_id: String,
    pub expected_credential_revision: u64,
    pub secret: CodexOAuthSecret,
    pub verified_account: CodexAccountProfile,
    pub next_refresh_at: Option<DateTime<Utc>>,
}

impl fmt::Debug for RotateCodexCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RotateCodexCredential")
            .field("account_id", &self.account_id)
            .field(
                "expected_credential_revision",
                &self.expected_credential_revision,
            )
            .field("secret", &self.secret)
            .field("verified_account", &self.verified_account)
            .field("next_refresh_at", &self.next_refresh_at)
            .finish()
    }
}

/// 运行时使用的 Cookie；值受 `secrecy` 保护。
pub struct RuntimeCodexCookie {
    pub name: String,
    pub value: SecretString,
    pub domain: String,
    pub path: String,
    pub host_only: bool,
    pub secure: bool,
    pub expires_at: Option<DateTime<Utc>>,
}

impl fmt::Debug for RuntimeCodexCookie {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeCodexCookie")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .field("domain", &self.domain)
            .field("path", &self.path)
            .field("host_only", &self.host_only)
            .field("secure", &self.secure)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// 一批 `Set-Cookie` CAS 写回的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodexCookieCaptureOutcome {
    pub credential_revision: Option<u64>,
    pub rejected: usize,
}

/// 单个已验证 Cookie 的 Provider JSON CAS 输入。
pub struct UpsertCodexCookie {
    pub account_id: String,
    pub expected_credential_revision: u64,
    pub response_origin: Url,
    pub domain_attribute: Option<String>,
    pub name: String,
    pub value: SecretString,
    pub path: String,
    pub secure: bool,
    pub expires_at: Option<DateTime<Utc>>,
    pub delete: bool,
}

impl fmt::Debug for UpsertCodexCookie {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpsertCodexCookie")
            .field("account_id", &self.account_id)
            .field(
                "expected_credential_revision",
                &self.expected_credential_revision,
            )
            .field("response_origin", &self.response_origin)
            .field("domain_attribute", &self.domain_attribute)
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .field("path", &self.path)
            .field("secure", &self.secure)
            .field("expires_at", &self.expires_at)
            .field("delete", &self.delete)
            .finish()
    }
}
