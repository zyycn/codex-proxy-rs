//! 账号导入导出逻辑（从 admin_accounts 内联导入解析）。

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::accounts::model::AccountStatus;
use crate::accounts::token_refresh::{jwt_expiry, JwtExpiry};

/// 账号导入条目。
#[derive(Debug, Clone)]
pub struct AccountImportEntry {
    /// 导入时指定的账号 ID。
    pub id: Option<String>,
    /// 邮箱。
    pub email: Option<String>,
    /// ChatGPT 账号 ID。
    pub account_id: Option<String>,
    /// ChatGPT 用户 ID。
    pub user_id: Option<String>,
    /// 管理标签。
    pub label: Option<String>,
    /// 订阅计划类型。
    pub plan_type: Option<String>,
    /// 访问令牌（明文）。
    pub token: Option<String>,
    /// 刷新令牌（明文）。
    pub refresh_token: Option<String>,
    /// 访问令牌过期时间（RFC 3339 字符串）。
    pub access_token_expires_at: Option<String>,
    /// 导入时的原始状态。
    pub status: Option<String>,
    /// Sub2api 导入时的缓存配额 JSON。
    pub cached_quota: Option<Value>,
    /// 配额抓取时间。
    pub quota_fetched_at: Option<String>,
    /// 是否需要执行额外配额校验。
    pub quota_verify_required: Option<bool>,
}

/// 账号导入来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountImportSource {
    /// 原生格式。
    Native,
    /// Sub2api 格式。
    Sub2api,
}

impl AccountImportSource {
    /// 返回来源的字符串标识。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Sub2api => "sub2api",
        }
    }
}

const ACCOUNT_IMPORT_ENVELOPE_KEYS: &[&str] =
    &["code", "message", "data", "requestId", "request_id"];
const ACCOUNT_IMPORT_CONTAINER_KEYS: &[&str] = &["sourceFormat", "source_format", "accounts"];
const ACCOUNT_IMPORT_ACCOUNT_KEYS: &[&str] = &[
    "id",
    "email",
    "accountId",
    "account_id",
    "userId",
    "user_id",
    "label",
    "planType",
    "plan_type",
    "token",
    "accessToken",
    "access_token",
    "refreshToken",
    "refresh_token",
    "accessTokenExpiresAt",
    "access_token_expires_at",
    "status",
    "addedAt",
    "added_at",
    "updatedAt",
    "updated_at",
];
const SUB2API_ACCOUNT_IMPORT_KEYS: &[&str] = &[
    "id",
    "email",
    "accountId",
    "account_id",
    "userId",
    "user_id",
    "label",
    "planType",
    "plan_type",
    "token",
    "accessToken",
    "access_token",
    "refreshToken",
    "refresh_token",
    "status",
    "addedAt",
    "added_at",
    "cachedQuota",
    "cached_quota",
    "quotaFetchedAt",
    "quota_fetched_at",
    "quotaVerifyRequired",
    "quota_verify_required",
    "proxyApiKey",
    "proxy_api_key",
    "usage",
];

/// 解析后的账号导入载荷。
#[derive(Debug, Clone)]
pub struct ParsedAccountImport {
    /// 来源格式。
    pub source: AccountImportSource,
    /// 导入条目。
    pub entries: Vec<AccountImportEntry>,
}

/// 解析账号导入载荷。
pub fn parse_account_import_payload(payload: &Value) -> Result<ParsedAccountImport, &'static str> {
    let payload = payload
        .get("data")
        .filter(|data| data.get("accounts").is_some())
        .map(|data| -> Result<&Value, &'static str> {
            ensure_account_import_keys(payload, ACCOUNT_IMPORT_ENVELOPE_KEYS)?;
            Ok(data)
        })
        .transpose()?
        .unwrap_or(payload);

    if let Some(accounts) = payload.get("accounts") {
        ensure_account_import_keys(payload, ACCOUNT_IMPORT_CONTAINER_KEYS)?;
        let accounts = accounts.as_array().ok_or("no importable accounts")?;
        let source = account_import_source(payload, accounts)?;
        return Ok(ParsedAccountImport {
            source,
            entries: parse_account_import_entries(accounts, source)?,
        });
    }
    if let Some(accounts) = payload.as_array() {
        let source = account_import_source(payload, accounts)?;
        return Ok(ParsedAccountImport {
            source,
            entries: parse_account_import_entries(accounts, source)?,
        });
    }

    let source = account_import_source(payload, std::slice::from_ref(payload))?;
    Ok(ParsedAccountImport {
        source,
        entries: account_import_entry_from_value(payload, source)?
            .into_iter()
            .collect(),
    })
}

fn parse_account_import_entries(
    accounts: &[Value],
    source: AccountImportSource,
) -> Result<Vec<AccountImportEntry>, &'static str> {
    let mut entries = Vec::new();
    for account in accounts {
        if let Some(entry) = account_import_entry_from_value(account, source)? {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn account_import_entry_from_value(
    value: &Value,
    source: AccountImportSource,
) -> Result<Option<AccountImportEntry>, &'static str> {
    let Some(account) = value.as_object() else {
        return Ok(None);
    };
    let allowed_keys = match source {
        AccountImportSource::Native => ACCOUNT_IMPORT_ACCOUNT_KEYS,
        AccountImportSource::Sub2api => SUB2API_ACCOUNT_IMPORT_KEYS,
    };
    if account
        .keys()
        .any(|key| !allowed_keys.contains(&key.as_str()))
    {
        return Err("no importable accounts");
    }

    let token = first_string(value, &["token", "accessToken", "access_token"]);
    let refresh_token = first_string(value, &["refreshToken", "refresh_token"]);
    if token.is_none() && refresh_token.is_none() {
        return Ok(None);
    }

    Ok(Some(AccountImportEntry {
        id: first_string(value, &["id"]),
        email: first_string(value, &["email"]),
        account_id: first_string(value, &["accountId", "account_id"]),
        user_id: first_string(value, &["userId", "user_id"]),
        label: first_string(value, &["label"]),
        plan_type: first_string(value, &["planType", "plan_type"]),
        token,
        refresh_token,
        access_token_expires_at: first_string(
            value,
            &["accessTokenExpiresAt", "access_token_expires_at"],
        ),
        status: first_string(value, &["status"]),
        cached_quota: (source == AccountImportSource::Sub2api)
            .then(|| first_value(value, &["cachedQuota", "cached_quota"]))
            .flatten(),
        quota_fetched_at: (source == AccountImportSource::Sub2api)
            .then(|| first_string(value, &["quotaFetchedAt", "quota_fetched_at"]))
            .flatten(),
        quota_verify_required: (source == AccountImportSource::Sub2api)
            .then(|| first_bool(value, &["quotaVerifyRequired", "quota_verify_required"]))
            .flatten(),
    }))
}

fn ensure_account_import_keys(value: &Value, allowed_keys: &[&str]) -> Result<(), &'static str> {
    let Some(object) = value.as_object() else {
        return Err("no importable accounts");
    };
    if object
        .keys()
        .all(|key| allowed_keys.contains(&key.as_str()))
    {
        Ok(())
    } else {
        Err("no importable accounts")
    }
}

fn account_import_source(
    value: &Value,
    accounts: &[Value],
) -> Result<AccountImportSource, &'static str> {
    if let Some(source_format) = first_string(value, &["sourceFormat", "source_format"]) {
        return match source_format.trim().to_ascii_lowercase().as_str() {
            "native" => Ok(AccountImportSource::Native),
            "sub2api" => Ok(AccountImportSource::Sub2api),
            _ => Err("no importable accounts"),
        };
    }

    if accounts.iter().any(account_import_entry_looks_sub2api) {
        Ok(AccountImportSource::Sub2api)
    } else {
        Ok(AccountImportSource::Native)
    }
}

fn account_import_entry_looks_sub2api(value: &Value) -> bool {
    let Some(account) = value.as_object() else {
        return false;
    };
    [
        "proxyApiKey",
        "proxy_api_key",
        "usage",
        "cachedQuota",
        "cached_quota",
        "quotaFetchedAt",
        "quota_fetched_at",
        "quotaVerifyRequired",
        "quota_verify_required",
    ]
    .iter()
    .any(|key| account.contains_key(*key))
}

/// 解析导入的状态字符串。
pub fn parse_account_import_status(status: Option<&str>) -> Result<AccountStatus, &'static str> {
    parse_account_status(status.unwrap_or("active"))
}

/// 规范化导入后的状态。
pub fn normalized_imported_account_status(
    status: AccountStatus,
    source: AccountImportSource,
    access_token: &str,
) -> AccountStatus {
    if source == AccountImportSource::Sub2api
        && status == AccountStatus::Active
        && jwt_expiry(access_token, Utc::now()) != JwtExpiry::Valid
    {
        AccountStatus::Expired
    } else {
        status
    }
}

/// 解析导入的 RFC 3339 时间。
pub fn parse_account_import_datetime(value: &str) -> Result<DateTime<Utc>, &'static str> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| "invalid RFC 3339 datetime")
}

/// 解析账号状态字符串。
pub fn parse_account_status(status: &str) -> Result<AccountStatus, &'static str> {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" => Ok(AccountStatus::Active),
        "disabled" => Ok(AccountStatus::Disabled),
        "expired" => Ok(AccountStatus::Expired),
        "quota_exhausted" => Ok(AccountStatus::QuotaExhausted),
        "refreshing" => Ok(AccountStatus::Refreshing),
        "banned" => Ok(AccountStatus::Banned),
        _ => Err("invalid account status"),
    }
}

/// 对管理端修改用的状态做严格校验。
pub fn parse_batch_account_status(status: &str) -> Result<AccountStatus, &'static str> {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" => Ok(AccountStatus::Active),
        "disabled" => Ok(AccountStatus::Disabled),
        _ => Err("invalid account status (only active/disabled allowed)"),
    }
}

/// 将 RefreshFailure 映射为账号状态。
pub fn refresh_failure_status(failure: &crate::accounts::oauth::RefreshFailure) -> AccountStatus {
    match failure {
        crate::accounts::oauth::RefreshFailure::InvalidGrant => AccountStatus::Disabled,
        crate::accounts::oauth::RefreshFailure::QuotaExhausted => AccountStatus::QuotaExhausted,
        crate::accounts::oauth::RefreshFailure::Banned => AccountStatus::Banned,
        crate::accounts::oauth::RefreshFailure::Disabled => AccountStatus::Disabled,
        crate::accounts::oauth::RefreshFailure::RetryableTransport => AccountStatus::Active,
        crate::accounts::oauth::RefreshFailure::Transport => AccountStatus::Active,
    }
}

/// 判断是否需要清除 next_refresh_at。
pub fn refresh_failure_status_clears_next_refresh_at(status: AccountStatus) -> bool {
    !matches!(status, AccountStatus::Active)
}

/// 标准化 Bearer token（去除 Bearer 前缀，去除空白）。
pub fn normalize_bearer_token(value: String) -> String {
    value
        .trim()
        .strip_prefix("Bearer ")
        .or_else(|| value.trim().strip_prefix("bearer "))
        .unwrap_or(value.trim())
        .trim()
        .to_string()
}

/// 标准化账号 ID。
pub fn normalized_account_id(id: Option<String>) -> String {
    normalize_nonempty(id).unwrap_or_else(|| format!("acct_{}", uuid::Uuid::new_v4().simple()))
}

/// 标准化标签。
pub fn normalize_label(value: Option<String>) -> Option<String> {
    normalize_nonempty(value)
}

/// 去除空字符串。
pub fn normalize_nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 去除空字符串（引用版本）。
pub fn normalize_nonempty_str(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

pub fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn first_bool(value: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_bool))
}

fn first_value(value: &Value, keys: &[&str]) -> Option<Value> {
    keys.iter()
        .find_map(|key| value.get(key).filter(|value| !value.is_null()))
        .cloned()
}
