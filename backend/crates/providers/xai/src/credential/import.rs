//! 已有 Grok Build OAuth token 的安全归一化边界。

use std::collections::HashSet;
use std::fmt;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::{Map, Value};
use url::Url;

use crate::credential::token::UnverifiedTokenSet;
use crate::{
    FailureClass, GROK_CLI_BASE_URL, GrokOAuthConfig, OAuthError, OFFICIAL_CLIENT_ID,
    OFFICIAL_SCOPES, SecretValue,
};

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

/// Provider-owned xAI OAuth 账号导入。
pub struct GrokOAuthImportDocument {
    entries: Vec<GrokOAuthImportEntry>,
}

impl GrokOAuthImportDocument {
    /// 从外部 JSON 提取 xAI OAuth 认证字段。
    ///
    /// 来源包装格式、代理、并发和其他展示 metadata 不参与认证，也不会影响导入。
    /// 实际 token 仍须通过官方 refresh/user-info 验证；API Key 不能混入 OAuth 条目。
    pub fn parse_json(document: &[u8]) -> Result<Self, GrokOAuthImportError> {
        if document.is_empty() || document.len() > MAX_IMPORT_DOCUMENT_BYTES {
            return Err(GrokOAuthImportError::InvalidField("document"));
        }
        let wire: Value = serde_json::from_slice(document)
            .map_err(|_| GrokOAuthImportError::InvalidField("document"))?;
        let accounts = import_accounts(&wire)?;
        if accounts.is_empty() || accounts.len() > MAX_IMPORT_ACCOUNTS {
            return Err(GrokOAuthImportError::InvalidField("document"));
        }
        let now = Utc::now();
        let mut entries = Vec::with_capacity(accounts.len());
        for (index, account) in accounts.into_iter().enumerate() {
            if let Some(entry) = parse_account_entry(account, index, now)? {
                entries.push(entry);
            }
        }
        if entries.is_empty() {
            return Err(GrokOAuthImportError::InvalidField("document"));
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

fn import_accounts(document: &Value) -> Result<Vec<&Value>, GrokOAuthImportError> {
    match document {
        Value::Array(accounts) => Ok(accounts.iter().collect()),
        Value::Object(object) => match object.get("accounts") {
            Some(Value::Array(accounts)) => Ok(accounts.iter().collect()),
            Some(_) => Err(GrokOAuthImportError::InvalidField("accounts")),
            None => Ok(vec![document]),
        },
        _ => Err(GrokOAuthImportError::InvalidField("document")),
    }
}

fn parse_account_entry(
    account: &Value,
    index: usize,
    now: DateTime<Utc>,
) -> Result<Option<GrokOAuthImportEntry>, GrokOAuthImportError> {
    let account = account
        .as_object()
        .ok_or(GrokOAuthImportError::InvalidField("account"))?;
    let credentials = account
        .get("credentials")
        .and_then(Value::as_object)
        .unwrap_or(account);
    let (access_token, refresh_token) = match (
        credential_value(credentials, &["access_token", "accessToken", "at"]),
        credential_value(credentials, &["refresh_token", "refreshToken", "rt"]),
    ) {
        (Some(access_token), Some(refresh_token)) => (access_token, refresh_token),
        (None, None) => return Ok(None),
        (None, Some(_)) => return Err(GrokOAuthImportError::InvalidField("access_token")),
        (Some(_), None) => return Err(GrokOAuthImportError::InvalidField("refresh_token")),
    };
    if ["api_key", "apiKey", "xai_api_key", "xaiApiKey"]
        .iter()
        .any(|field| credentials.contains_key(*field))
    {
        return Err(GrokOAuthImportError::InvalidField("account"));
    }

    let email = [
        credential_value(credentials, &["email"]),
        display_value(account, &["email"]),
        account
            .get("extra")
            .and_then(Value::as_object)
            .and_then(|extra| display_value(extra, &["email"])),
    ]
    .into_iter()
    .flatten()
    .find(|value| valid_display_value(value, MAX_EMAIL_BYTES))
    .map(str::to_owned);
    let name = display_value(account, &["name", "label"])
        .filter(|value| valid_display_value(value, MAX_ACCOUNT_NAME_BYTES))
        .map(str::to_owned)
        .or_else(|| email.clone())
        .unwrap_or_else(|| format!("xAI OAuth account {}", index + 1));
    let expires_at = credential_value(
        credentials,
        &[
            "expires_at",
            "expiresAt",
            "access_token_expires_at",
            "accessTokenExpiresAt",
        ],
    )
    .and_then(parse_source_expiry)
    .filter(|expires_at| *expires_at > now && *expires_at - now <= MAX_DECLARED_LIFETIME)
    .unwrap_or_else(|| now + ChronoDuration::seconds(1));
    let tokens = match credential_value(credentials, &["id_token", "idToken"]) {
        Some(id_token) => GrokOAuthImportTokens::new(
            SecretValue::new(access_token.to_owned()),
            SecretValue::new(refresh_token.to_owned()),
            SecretValue::new(id_token.to_owned()),
        ),
        None => GrokOAuthImportTokens::without_id_token(
            SecretValue::new(access_token.to_owned()),
            SecretValue::new(refresh_token.to_owned()),
        ),
    };
    let metadata = GrokOAuthImportMetadata::new(
        "Bearer".to_owned(),
        OFFICIAL_CLIENT_ID.to_owned(),
        OFFICIAL_SCOPES.join(" "),
        GROK_CLI_BASE_URL.to_owned(),
        now,
        expires_at,
    );

    Ok(Some(GrokOAuthImportEntry {
        name,
        email,
        candidate: GrokOAuthImportCandidate::new(tokens, metadata),
    }))
}

fn credential_value<'a>(credentials: &'a Map<String, Value>, fields: &[&str]) -> Option<&'a str> {
    display_value(credentials, fields).or_else(|| {
        credentials
            .get("token")
            .and_then(Value::as_object)
            .and_then(|token| display_value(token, fields))
    })
}

fn display_value<'a>(object: &'a Map<String, Value>, fields: &[&str]) -> Option<&'a str> {
    fields
        .iter()
        .find_map(|field| object.get(*field).and_then(Value::as_str))
}

fn parse_source_expiry(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
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
