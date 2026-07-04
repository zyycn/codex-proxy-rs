//! 账号导入逻辑。

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use crate::upstream::accounts::model::AccountStatus;
use crate::upstream::accounts::token_refresh::{jwt_expiry, JwtExpiry};

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
    /// Sub2API 导入时的缓存配额 JSON。
    pub cached_quota: Option<Value>,
    /// 配额抓取时间。
    pub quota_fetched_at: Option<String>,
    /// 是否需要执行额外配额校验。
    pub quota_verify_required: Option<bool>,
}

/// 账号导入来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountImportSource {
    /// CPR 格式。
    Cpr,
    /// Sub2API 格式。
    Sub2Api,
    /// CLIProxyAPI auth 文件格式。
    CliProxyApi,
}

impl AccountImportSource {
    /// 返回来源的字符串标识。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpr => "cpr",
            Self::Sub2Api => "sub2api",
            Self::CliProxyApi => "cliproxyapi",
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
    "at",
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
    let payload = admin_envelope_data(payload)?.unwrap_or(payload);
    match explicit_account_import_source(payload)? {
        Some(AccountImportSource::Cpr) => parse_cpr_payload(payload),
        Some(AccountImportSource::Sub2Api) => parse_sub2api_payload(payload),
        Some(AccountImportSource::CliProxyApi) => parse_cli_proxy_api_payload(payload),
        None if account_import_payload_looks_sub2api(payload) => parse_sub2api_payload(payload),
        None if account_import_payload_looks_cli_proxy_api(payload) => {
            parse_cli_proxy_api_payload(payload)
        }
        None => parse_cpr_payload(payload),
    }
}

fn admin_envelope_data(payload: &Value) -> Result<Option<&Value>, &'static str> {
    let Some(data) = payload
        .get("data")
        .filter(|data| data.get("accounts").is_some())
    else {
        return Ok(None);
    };
    ensure_account_import_keys(payload, ACCOUNT_IMPORT_ENVELOPE_KEYS)?;
    Ok(Some(data))
}

fn explicit_account_import_source(
    value: &Value,
) -> Result<Option<AccountImportSource>, &'static str> {
    let Some(source_format) = first_string(value, &["sourceFormat", "source_format"]) else {
        return Ok(None);
    };
    match source_format.trim().to_ascii_lowercase().as_str() {
        "cpr" => Ok(Some(AccountImportSource::Cpr)),
        "sub2api" => Ok(Some(AccountImportSource::Sub2Api)),
        "cliproxyapi" | "cli_proxy_api" | "cli-proxy-api" | "cpa" => {
            Ok(Some(AccountImportSource::CliProxyApi))
        }
        _ => Err("no importable accounts"),
    }
}

fn parse_cpr_payload(payload: &Value) -> Result<ParsedAccountImport, &'static str> {
    Ok(ParsedAccountImport {
        source: AccountImportSource::Cpr,
        entries: parse_account_entries(
            payload,
            cpr_account_import_entry_from_value,
            Some(ACCOUNT_IMPORT_CONTAINER_KEYS),
        )?,
    })
}

fn parse_sub2api_payload(payload: &Value) -> Result<ParsedAccountImport, &'static str> {
    Ok(ParsedAccountImport {
        source: AccountImportSource::Sub2Api,
        entries: parse_account_entries(payload, sub2api_account_import_entry_from_value, None)?,
    })
}

fn parse_cli_proxy_api_payload(payload: &Value) -> Result<ParsedAccountImport, &'static str> {
    Ok(ParsedAccountImport {
        source: AccountImportSource::CliProxyApi,
        entries: parse_account_entries(
            payload,
            cli_proxy_api_account_import_entry_from_value,
            None,
        )?,
    })
}

type AccountImportEntryParser = fn(&Value) -> Result<Option<AccountImportEntry>, &'static str>;

fn parse_account_entries(
    payload: &Value,
    parser: AccountImportEntryParser,
    container_keys: Option<&[&str]>,
) -> Result<Vec<AccountImportEntry>, &'static str> {
    if let Some(accounts) = payload.get("accounts") {
        if let Some(container_keys) = container_keys {
            ensure_account_import_keys(payload, container_keys)?;
        }
        let accounts = accounts.as_array().ok_or("no importable accounts")?;
        return parse_entries(accounts, parser);
    }
    if let Some(accounts) = payload.as_array() {
        return parse_entries(accounts, parser);
    }
    parser(payload).map(option_to_vec)
}

fn parse_entries(
    accounts: &[Value],
    parser: AccountImportEntryParser,
) -> Result<Vec<AccountImportEntry>, &'static str> {
    let mut entries = Vec::new();
    for account in accounts {
        if let Some(entry) = parser(account)? {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn option_to_vec<T>(value: Option<T>) -> Vec<T> {
    value.into_iter().collect()
}

fn cpr_account_import_entry_from_value(
    value: &Value,
) -> Result<Option<AccountImportEntry>, &'static str> {
    let Some(account) = value.as_object() else {
        return Ok(None);
    };
    if account
        .keys()
        .any(|key| !ACCOUNT_IMPORT_ACCOUNT_KEYS.contains(&key.as_str()))
    {
        return Err("no importable accounts");
    }

    let token = first_string(value, &["token", "at", "accessToken", "access_token"]);
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
        cached_quota: None,
        quota_fetched_at: None,
        quota_verify_required: None,
    }))
}

fn sub2api_account_import_entry_from_value(
    value: &Value,
) -> Result<Option<AccountImportEntry>, &'static str> {
    if sub2api_account_backup_entry(value) {
        return Ok(sub2api_backup_account_entry(value));
    }
    Ok(sub2api_codex_session_or_flat_entry(value))
}

fn sub2api_backup_account_entry(value: &Value) -> Option<AccountImportEntry> {
    let account = value.as_object()?;
    if !optional_string_field_matches(account, "platform", "openai")
        || !optional_string_field_matches(account, "type", "oauth")
    {
        return None;
    }
    let credentials = value.get("credentials")?;
    let token = first_path_string(
        credentials,
        [
            &["access_token"],
            &["accessToken"],
            &["at"],
            &["token", "access_token"],
            &["token", "accessToken"],
            &["token", "at"],
        ],
    );
    let refresh_token = first_path_string(
        credentials,
        [
            &["refresh_token"],
            &["refreshToken"],
            &["rt"],
            &["token", "refresh_token"],
            &["token", "refreshToken"],
            &["token", "rt"],
        ],
    );
    if token.is_none() && refresh_token.is_none() {
        return None;
    }

    Some(AccountImportEntry {
        id: first_string(value, &["id"]),
        email: first_path_string(credentials, [&["email"], &["user", "email"]]),
        account_id: first_path_string(
            credentials,
            [
                &["chatgpt_account_id"],
                &["chatgptAccountId"],
                &["account_id"],
                &["accountId"],
                &["account", "id"],
                &["account", "account_id"],
                &["account", "chatgpt_account_id"],
            ],
        ),
        user_id: first_path_string(
            credentials,
            [
                &["chatgpt_user_id"],
                &["chatgptUserId"],
                &["user_id"],
                &["userId"],
                &["user", "id"],
            ],
        ),
        label: first_string(value, &["label", "name"]),
        plan_type: first_path_string(credentials, [&["plan_type"], &["planType"]]),
        token,
        refresh_token,
        access_token_expires_at: first_datetime_string(
            credentials,
            [&["expires_at"], &["expiresAt"], &["expired"], &["expire"]],
        ),
        status: first_string(value, &["status"]),
        cached_quota: first_value(value, &["cachedQuota", "cached_quota"]),
        quota_fetched_at: first_string(value, &["quotaFetchedAt", "quota_fetched_at"]),
        quota_verify_required: first_bool(value, &["quotaVerifyRequired", "quota_verify_required"]),
    })
}

fn sub2api_codex_session_or_flat_entry(value: &Value) -> Option<AccountImportEntry> {
    let _account = value.as_object()?;
    let token = first_path_string(
        value,
        [
            &["tokens", "access_token"],
            &["tokens", "accessToken"],
            &["tokens", "at"],
            &["access_token"],
            &["accessToken"],
            &["token"],
            &["at"],
        ],
    );
    let refresh_token = first_path_string(
        value,
        [
            &["tokens", "refresh_token"],
            &["tokens", "refreshToken"],
            &["tokens", "rt"],
            &["refresh_token"],
            &["refreshToken"],
            &["rt"],
        ],
    );
    if token.is_none() && refresh_token.is_none() {
        return None;
    }

    Some(AccountImportEntry {
        id: first_string(value, &["id"]),
        email: first_path_string(value, [&["email"], &["user", "email"]]),
        account_id: first_path_string(
            value,
            [
                &["chatgpt_account_id"],
                &["chatgptAccountId"],
                &["account_id"],
                &["accountId"],
                &["account", "id"],
                &["account", "account_id"],
                &["account", "chatgpt_account_id"],
            ],
        ),
        user_id: first_path_string(
            value,
            [
                &["chatgpt_user_id"],
                &["chatgptUserId"],
                &["user_id"],
                &["userId"],
                &["user", "id"],
            ],
        ),
        label: first_path_string(value, [&["label"], &["name"], &["user", "name"]]),
        plan_type: first_path_string(
            value,
            [
                &["plan_type"],
                &["planType"],
                &["account", "plan_type"],
                &["account", "planType"],
            ],
        ),
        token,
        refresh_token,
        access_token_expires_at: first_datetime_string(
            value,
            [
                &["tokens", "expires_at"],
                &["tokens", "expiresAt"],
                &["expires_at"],
                &["expiresAt"],
            ],
        ),
        status: first_string(value, &["status"]),
        cached_quota: first_value(value, &["cachedQuota", "cached_quota"]),
        quota_fetched_at: first_string(value, &["quotaFetchedAt", "quota_fetched_at"]),
        quota_verify_required: first_bool(value, &["quotaVerifyRequired", "quota_verify_required"]),
    })
}

fn cli_proxy_api_account_import_entry_from_value(
    value: &Value,
) -> Result<Option<AccountImportEntry>, &'static str> {
    let Some(account) = value.as_object() else {
        return Ok(None);
    };
    if !cli_proxy_api_provider_is_codex(account) {
        return Ok(None);
    }

    let token = first_path_string(
        value,
        [
            &["access_token"],
            &["accessToken"],
            &["at"],
            &["token", "access_token"],
            &["token", "accessToken"],
            &["token", "at"],
            &["token"],
        ],
    );
    let refresh_token = first_path_string(
        value,
        [
            &["refresh_token"],
            &["refreshToken"],
            &["rt"],
            &["token", "refresh_token"],
            &["token", "refreshToken"],
            &["token", "rt"],
        ],
    );
    if token.is_none() && refresh_token.is_none() {
        return Ok(None);
    }

    Ok(Some(AccountImportEntry {
        id: None,
        email: first_string(value, &["email"]),
        account_id: first_string(
            value,
            &["chatgpt_account_id", "chatgptAccountId", "account_id"],
        ),
        user_id: first_string(
            value,
            &["chatgpt_user_id", "chatgptUserId", "user_id", "userId"],
        ),
        label: first_string(value, &["label", "name"]),
        plan_type: first_path_string(
            value,
            [&["plan_type"], &["planType"], &["attributes", "plan_type"]],
        ),
        token,
        refresh_token,
        access_token_expires_at: first_datetime_string(
            value,
            [
                &["expired"],
                &["expire"],
                &["expires_at"],
                &["expiresAt"],
                &["expiry"],
                &["expires"],
            ],
        ),
        status: cli_proxy_api_status(account),
        cached_quota: None,
        quota_fetched_at: None,
        quota_verify_required: None,
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

fn account_import_payload_looks_sub2api(value: &Value) -> bool {
    if value.get("proxies").is_some() || value.get("exported_at").is_some() {
        return true;
    }
    if let Some(accounts) = value.get("accounts").and_then(Value::as_array) {
        return accounts.iter().any(sub2api_account_looks_known);
    }
    if let Some(accounts) = value.as_array() {
        return accounts.iter().any(sub2api_account_looks_known);
    }
    sub2api_account_looks_known(value)
}

fn sub2api_account_looks_known(value: &Value) -> bool {
    let Some(account) = value.as_object() else {
        return false;
    };
    sub2api_account_backup_entry(value)
        || value.get("tokens").is_some()
        || [
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

fn sub2api_account_backup_entry(value: &Value) -> bool {
    value.get("credentials").is_some()
        && (value.get("platform").is_some() || value.get("type").is_some())
}

fn account_import_payload_looks_cli_proxy_api(value: &Value) -> bool {
    if let Some(accounts) = value.get("accounts").and_then(Value::as_array) {
        return accounts.iter().any(cli_proxy_api_account_looks_known);
    }
    if let Some(accounts) = value.as_array() {
        return accounts.iter().any(cli_proxy_api_account_looks_known);
    }
    cli_proxy_api_account_looks_known(value)
}

fn cli_proxy_api_account_looks_known(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(cli_proxy_api_provider_is_codex)
}

fn cli_proxy_api_provider_is_codex(account: &Map<String, Value>) -> bool {
    let provider = account
        .get("type")
        .or_else(|| account.get("provider"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(provider.as_str(), "codex" | "openai")
}

fn cli_proxy_api_status(account: &Map<String, Value>) -> Option<String> {
    account
        .get("disabled")
        .and_then(Value::as_bool)
        .filter(|disabled| *disabled)
        .map(|_| "disabled".to_string())
}

fn optional_string_field_matches(account: &Map<String, Value>, key: &str, expected: &str) -> bool {
    account
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none_or(|value| value.eq_ignore_ascii_case(expected))
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
    if source == AccountImportSource::Sub2Api
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

/// 判断是否需要清除 next_refresh_at。
pub fn refresh_failure_status_clears_next_refresh_at(status: AccountStatus) -> bool {
    !matches!(status, AccountStatus::Active)
}

/// 标准化 Bearer token（去除 Bearer 前缀，去除空白）。
pub fn normalize_bearer_token(value: &str) -> String {
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

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn first_path_string<const N: usize>(value: &Value, paths: [&[&str]; N]) -> Option<String> {
    paths
        .iter()
        .find_map(|path| value_at_path(value, path).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn first_datetime_string<const N: usize>(value: &Value, paths: [&[&str]; N]) -> Option<String> {
    paths
        .iter()
        .find_map(|path| value_at_path(value, path))
        .and_then(datetime_value_to_rfc3339)
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

fn value_at_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn datetime_value_to_rfc3339(value: &Value) -> Option<String> {
    if let Some(value) = value.as_str() {
        return normalize_nonempty_str(Some(value)).map(ToString::to_string);
    }
    if let Some(value) = value.as_i64() {
        return DateTime::<Utc>::from_timestamp(value, 0).map(|time| time.to_rfc3339());
    }
    if let Some(value) = value.as_u64().and_then(|value| i64::try_from(value).ok()) {
        return DateTime::<Utc>::from_timestamp(value, 0).map(|time| time.to_rfc3339());
    }
    value
        .as_f64()
        .filter(|value| value.is_finite())
        .map(|value| value.trunc() as i64)
        .and_then(|value| DateTime::<Utc>::from_timestamp(value, 0))
        .map(|time| time.to_rfc3339())
}
