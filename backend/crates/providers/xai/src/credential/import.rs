//! 已有 Grok Build OAuth token 的安全归一化边界。

use std::collections::{HashMap, HashSet};
use std::fmt;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use crate::credential::token::UnverifiedTokenSet;
use crate::{FailureClass, GROK_CLI_BASE_URL, GrokOAuthConfig, OAuthError, SecretValue};

const MAX_ACCESS_TOKEN_BYTES: usize = 16 * 1024;
const MAX_REFRESH_TOKEN_BYTES: usize = 64 * 1024;
const MAX_ID_TOKEN_BYTES: usize = 64 * 1024;
const MAX_CLIENT_ID_BYTES: usize = 128;
const MAX_SCOPE_BYTES: usize = 4 * 1024;
const MAX_BASE_URL_BYTES: usize = 2 * 1024;
const MAX_EXPORTED_AT_FUTURE_SKEW: ChronoDuration = ChronoDuration::minutes(5);
const MIN_REMAINING_LIFETIME: ChronoDuration = ChronoDuration::seconds(30);
const MAX_DECLARED_LIFETIME: ChronoDuration = ChronoDuration::hours(24);
const REQUIRED_SCOPES: &[&str] = &["openid", "offline_access", "grok-cli:access", "api:access"];
const MAX_IMPORT_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_IMPORT_ACCOUNTS: usize = 200;
const MAX_ACCOUNT_NAME_BYTES: usize = 512;
const MAX_EMAIL_BYTES: usize = 2_048;

/// 管理导入 adapter 归一化后的 OAuth 候选；所有秘密字段均从 `Debug` 隐去。
pub struct GrokOAuthImportCandidate {
    access_token: SecretValue,
    refresh_token: SecretValue,
    id_token: Option<SecretValue>,
    token_type: String,
    client_id: String,
    scope: String,
    inference_base_url: String,
    exported_at: DateTime<Utc>,
    access_token_expires_at: DateTime<Utc>,
}

/// 导入候选的三个 OAuth secret；该类型不实现 `Debug` 或序列化。
pub struct GrokOAuthImportTokens {
    access_token: SecretValue,
    refresh_token: SecretValue,
    id_token: Option<SecretValue>,
}

impl GrokOAuthImportTokens {
    #[must_use]
    pub fn new(
        access_token: SecretValue,
        refresh_token: SecretValue,
        id_token: SecretValue,
    ) -> Self {
        Self {
            access_token,
            refresh_token,
            id_token: Some(id_token),
        }
    }

    /// 构造不含 ID token 的导入；仅允许过期 AT 经 RT 刷新后走 user-info 验证。
    #[must_use]
    pub fn without_id_token(access_token: SecretValue, refresh_token: SecretValue) -> Self {
        Self {
            access_token,
            refresh_token,
            id_token: None,
        }
    }
}

/// 导入候选的非身份 metadata；client 与 scope 仍按敏感材料处理，不实现 `Debug`。
pub struct GrokOAuthImportMetadata {
    token_type: String,
    client_id: String,
    scope: String,
    inference_base_url: String,
    exported_at: DateTime<Utc>,
    access_token_expires_at: DateTime<Utc>,
}

pub(crate) struct ValidatedGrokOAuthImport {
    pub(crate) tokens: UnverifiedTokenSet,
    pub(crate) requires_refresh: bool,
    pub(crate) scope: String,
}

impl GrokOAuthImportMetadata {
    #[must_use]
    pub fn new(
        token_type: String,
        client_id: String,
        scope: String,
        inference_base_url: String,
        exported_at: DateTime<Utc>,
        access_token_expires_at: DateTime<Utc>,
    ) -> Self {
        Self {
            token_type,
            client_id,
            scope,
            inference_base_url,
            exported_at,
            access_token_expires_at,
        }
    }
}

/// OAuth 账号文档中的一个 xAI account。
pub struct GrokOAuthImportEntry {
    name: String,
    email: Option<String>,
    candidate: GrokOAuthImportCandidate,
}

impl GrokOAuthImportEntry {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn email(&self) -> Option<&str> {
        self.email.as_deref()
    }

    #[must_use]
    pub fn into_candidate(self) -> GrokOAuthImportCandidate {
        self.candidate
    }
}

impl fmt::Debug for GrokOAuthImportEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokOAuthImportEntry")
            .field("name", &self.name)
            .field("email", &self.email.as_ref().map(|_| "[REDACTED]"))
            .field("candidate", &self.candidate)
            .finish()
    }
}

/// Provider-owned xAI OAuth 账号文档。
pub struct GrokOAuthImportDocument {
    entries: Vec<GrokOAuthImportEntry>,
}

impl GrokOAuthImportDocument {
    /// 解析完整 OAuth 账号文档；未知字段、代理、API Key 或非 xAI OAuth 项均拒绝。
    pub fn parse_json(document: &[u8]) -> Result<Self, GrokOAuthImportError> {
        if document.is_empty() || document.len() > MAX_IMPORT_DOCUMENT_BYTES {
            return Err(GrokOAuthImportError::InvalidField("document"));
        }
        let wire: OAuthAccountDocumentWire = serde_json::from_slice(document)
            .map_err(|_| GrokOAuthImportError::InvalidField("document"))?;
        if wire
            .kind
            .as_deref()
            .is_some_and(|kind| kind != "external-account-bundle")
            || wire.version.is_some_and(|version| version != 1)
            || !wire.proxies.is_empty()
            || wire.accounts.is_empty()
            || wire.accounts.len() > MAX_IMPORT_ACCOUNTS
        {
            return Err(GrokOAuthImportError::InvalidField("document"));
        }
        let mut names = HashSet::new();
        let mut entries = Vec::with_capacity(wire.accounts.len());
        for account in wire.accounts {
            if account.credentials.extra_fields.keys().any(|field| {
                matches!(
                    field.as_str(),
                    "api_key" | "apiKey" | "xai_api_key" | "xaiApiKey"
                )
            }) {
                return Err(GrokOAuthImportError::InvalidField("account"));
            }
            if account.platform != "grok"
                || account.kind != "oauth"
                || account.concurrency == 0
                || account.priority == 0
                || !valid_display_value(&account.name, MAX_ACCOUNT_NAME_BYTES)
                || !names.insert(account.name.clone())
            {
                return Err(GrokOAuthImportError::InvalidField("account"));
            }
            let email = match (account.credentials.email, account.extra.email) {
                (Some(left), Some(right)) if left != right => {
                    return Err(GrokOAuthImportError::InvalidField("email"));
                }
                (Some(email), _) | (_, Some(email)) => Some(email),
                (None, None) => None,
            };
            if email
                .as_deref()
                .is_some_and(|value| !valid_display_value(value, MAX_EMAIL_BYTES))
            {
                return Err(GrokOAuthImportError::InvalidField("email"));
            }
            let tokens = match account.credentials.id_token {
                Some(id_token) => GrokOAuthImportTokens::new(
                    SecretValue::new(account.credentials.access_token),
                    SecretValue::new(account.credentials.refresh_token),
                    SecretValue::new(id_token),
                ),
                None => GrokOAuthImportTokens::without_id_token(
                    SecretValue::new(account.credentials.access_token),
                    SecretValue::new(account.credentials.refresh_token),
                ),
            };
            let metadata = GrokOAuthImportMetadata::new(
                account.credentials.token_type,
                account.credentials.client_id,
                account.credentials.scope,
                account.credentials.base_url,
                wire.exported_at,
                account.credentials.expires_at,
            );
            entries.push(GrokOAuthImportEntry {
                name: account.name,
                email,
                candidate: GrokOAuthImportCandidate::new(tokens, metadata),
            });
        }
        Ok(Self { entries })
    }

    #[must_use]
    pub fn into_entries(self) -> Vec<GrokOAuthImportEntry> {
        self.entries
    }
}

impl fmt::Debug for GrokOAuthImportDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokOAuthImportDocument")
            .field("account_count", &self.entries.len())
            .field("credentials", &"[REDACTED]")
            .finish()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OAuthAccountDocumentWire {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    version: Option<u32>,
    exported_at: DateTime<Utc>,
    accounts: Vec<OAuthAccountWire>,
    proxies: Vec<Value>,
    #[serde(default, rename = "skipped_shadows")]
    _skipped_shadows: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OAuthAccountWire {
    name: String,
    #[serde(default, rename = "notes")]
    _notes: Option<String>,
    platform: String,
    #[serde(rename = "type")]
    kind: String,
    credentials: OAuthCredentialsWire,
    concurrency: u32,
    priority: u32,
    #[serde(default, rename = "rate_multiplier")]
    _rate_multiplier: Option<Value>,
    #[serde(default, rename = "expires_at")]
    _expires_at: Option<i64>,
    #[serde(default, rename = "auto_pause_on_expired")]
    _auto_pause_on_expired: Option<bool>,
    #[serde(default, rename = "proxy_key")]
    _proxy_key: Option<String>,
    #[serde(default)]
    extra: OAuthAccountExtraWire,
}

#[derive(Deserialize)]
struct OAuthCredentialsWire {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    id_token: Option<String>,
    token_type: String,
    expires_at: DateTime<Utc>,
    email: Option<String>,
    base_url: String,
    client_id: String,
    scope: String,
    #[serde(flatten)]
    extra_fields: HashMap<String, Value>,
}

#[derive(Default, Deserialize)]
struct OAuthAccountExtraWire {
    #[serde(default)]
    email: Option<String>,
    #[serde(flatten)]
    _extra_fields: HashMap<String, Value>,
}

impl GrokOAuthImportCandidate {
    /// 构造尚未获得信任的导入候选。
    #[must_use]
    pub fn new(tokens: GrokOAuthImportTokens, metadata: GrokOAuthImportMetadata) -> Self {
        Self {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            id_token: tokens.id_token,
            token_type: metadata.token_type,
            client_id: metadata.client_id,
            scope: metadata.scope,
            inference_base_url: metadata.inference_base_url,
            exported_at: metadata.exported_at,
            access_token_expires_at: metadata.access_token_expires_at,
        }
    }

    pub(crate) fn validate(
        self,
        config: &GrokOAuthConfig,
        now: DateTime<Utc>,
    ) -> Result<ValidatedGrokOAuthImport, GrokOAuthImportError> {
        validate_secret(
            self.access_token.expose(),
            MAX_ACCESS_TOKEN_BYTES,
            "access_token",
        )?;
        validate_secret(
            self.refresh_token.expose(),
            MAX_REFRESH_TOKEN_BYTES,
            "refresh_token",
        )?;
        if let Some(id_token) = self.id_token.as_ref() {
            validate_secret(id_token.expose(), MAX_ID_TOKEN_BYTES, "id_token")?;
        }
        if !self.token_type.eq_ignore_ascii_case("bearer") {
            return Err(GrokOAuthImportError::InvalidField("token_type"));
        }
        if self.client_id.len() > MAX_CLIENT_ID_BYTES || self.client_id != config.client_id() {
            return Err(GrokOAuthImportError::InvalidField("client_id"));
        }
        validate_scope(&self.scope)?;
        validate_base_url(&self.inference_base_url)?;
        if self.exported_at > now + MAX_EXPORTED_AT_FUTURE_SKEW
            || self.access_token_expires_at <= self.exported_at
            || self.access_token_expires_at - self.exported_at > MAX_DECLARED_LIFETIME
        {
            return Err(GrokOAuthImportError::InvalidField("expires_at"));
        }
        let remaining = self.access_token_expires_at - now;
        let requires_refresh = remaining <= MIN_REMAINING_LIFETIME;
        let expires_in = if requires_refresh {
            None
        } else {
            Some(
                remaining
                    .to_std()
                    .map_err(|_| GrokOAuthImportError::InvalidField("expires_at"))?,
            )
        };
        Ok(ValidatedGrokOAuthImport {
            tokens: UnverifiedTokenSet {
                access_token: self.access_token,
                refresh_token: Some(self.refresh_token),
                id_token: self.id_token,
                expires_in,
            },
            requires_refresh,
            scope: self.scope,
        })
    }
}

impl fmt::Debug for GrokOAuthImportCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokOAuthImportCandidate")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("id_token", &"[REDACTED]")
            .field("token_type", &self.token_type)
            .field("client_id", &"[REDACTED]")
            .field("scope", &"[REDACTED]")
            .field("inference_base_url", &self.inference_base_url)
            .field("exported_at", &self.exported_at)
            .field("access_token_expires_at", &self.access_token_expires_at)
            .finish()
    }
}

/// OAuth 导入失败；错误中只保留固定字段名与低基数原因。
#[derive(Debug, thiserror::Error)]
pub enum GrokOAuthImportError {
    #[error("invalid imported OAuth field `{0}`")]
    InvalidField(&'static str),
    #[error(transparent)]
    OAuth(#[from] OAuthError),
}

impl GrokOAuthImportError {
    #[must_use]
    pub fn class(&self) -> FailureClass {
        match self {
            Self::OAuth(error) => error.class(),
            Self::InvalidField(_) => FailureClass::Security,
        }
    }
}

fn validate_secret(
    value: &str,
    max_bytes: usize,
    field: &'static str,
) -> Result<(), GrokOAuthImportError> {
    if value.is_empty()
        || value.len() > max_bytes
        || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(GrokOAuthImportError::InvalidField(field));
    }
    Ok(())
}

fn valid_display_value(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.chars().all(|character| {
            !character.is_control() && character != '\u{2028}' && character != '\u{2029}'
        })
}

fn validate_scope(scope: &str) -> Result<(), GrokOAuthImportError> {
    if scope.is_empty()
        || scope.len() > MAX_SCOPE_BYTES
        || !scope.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
    {
        return Err(GrokOAuthImportError::InvalidField("scope"));
    }
    let mut values = HashSet::new();
    for value in scope.split_ascii_whitespace() {
        if value.len() > 128 || !values.insert(value) {
            return Err(GrokOAuthImportError::InvalidField("scope"));
        }
    }
    if !REQUIRED_SCOPES
        .iter()
        .all(|required| values.contains(required))
    {
        return Err(GrokOAuthImportError::InvalidField("scope"));
    }
    Ok(())
}

fn validate_base_url(value: &str) -> Result<(), GrokOAuthImportError> {
    if value.is_empty() || value.len() > MAX_BASE_URL_BYTES {
        return Err(GrokOAuthImportError::InvalidField("base_url"));
    }
    let mut url = Url::parse(value).map_err(|_| GrokOAuthImportError::InvalidField("base_url"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(GrokOAuthImportError::InvalidField("base_url"));
    }
    let normalized_path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&normalized_path);
    if url.as_str() != GROK_CLI_BASE_URL {
        return Err(GrokOAuthImportError::InvalidField("base_url"));
    }
    Ok(())
}
