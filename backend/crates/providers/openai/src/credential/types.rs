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

/// Codex 对 `provider_credentials_json` 的完整明文 schema。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexCredentialData {
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

impl fmt::Debug for CodexCredentialData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCredentialData")
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
