//! SQLite Cookie 存储。

use chrono::{DateTime, Duration, Utc};
use secrecy::{ExposeSecret, SecretString};
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use codex_proxy_platform::crypto::{CryptoError, SecretBox};

const DEFAULT_COOKIE_DOMAIN: &str = "chatgpt.com";
const CAPTURABLE_COOKIES: &[&str] = &["cf_clearance"];

/// SQLite Cookie 存储错误。
#[derive(Debug, Error)]
pub enum SqliteCookieStoreError {
    /// 数据库错误。
    #[error("sqlite cookie store database error: {0}")]
    Database(#[from] sqlx::Error),
    /// 加解密错误。
    #[error("sqlite cookie store crypto error: {0}")]
    Crypto(#[from] CryptoError),
}

/// SQLite Cookie 存储结果。
pub type SqliteCookieStoreResult<T> = Result<T, SqliteCookieStoreError>;

/// SQLite Cookie 存储。
#[derive(Clone)]
pub struct SqliteCookieStore {
    pool: SqlitePool,
    secret_box: SecretBox,
}

impl SqliteCookieStore {
    /// 构造存储。
    pub fn new(pool: SqlitePool, secret_box: SecretBox) -> Self {
        Self { pool, secret_box }
    }

    /// 返回底层连接池。
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// 检查账号是否存在。
    pub async fn account_exists(&self, account_id: &str) -> SqliteCookieStoreResult<bool> {
        let row = sqlx::query("select 1 from accounts where id = ?")
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    /// 捕获上游 `Set-Cookie` 响应头中允许持久化的 Cookie。
    pub async fn capture_set_cookie(
        &self,
        account_id: &str,
        raw: &str,
    ) -> SqliteCookieStoreResult<()> {
        let Some(parsed) = parse_set_cookie(raw) else {
            return Ok(());
        };
        if !CAPTURABLE_COOKIES.contains(&parsed.name.as_str()) {
            return Ok(());
        }
        self.upsert_cookie(account_id, parsed).await
    }

    /// 将 Cookie 请求头写入账号 Cookie 存储。
    pub async fn set_cookie_header(
        &self,
        account_id: &str,
        raw: &str,
    ) -> SqliteCookieStoreResult<usize> {
        let parsed = parse_cookie_header(raw);
        let count = parsed.len();
        for cookie in parsed {
            self.upsert_cookie(account_id, cookie).await?;
        }
        Ok(count)
    }

    /// 为请求域名读取账号 Cookie 请求头。
    pub async fn cookie_header(
        &self,
        account_id: &str,
        request_domain: &str,
    ) -> SqliteCookieStoreResult<Option<String>> {
        self.cookie_header_for_request(account_id, request_domain, "/")
            .await
    }

    /// 为请求域名和路径读取账号 Cookie 请求头。
    pub async fn cookie_header_for_request(
        &self,
        account_id: &str,
        request_domain: &str,
        request_path: &str,
    ) -> SqliteCookieStoreResult<Option<String>> {
        let rows = sqlx::query(
            "select domain, name, value_cipher, path, expires_at from account_cookies where account_id = ?",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;
        let mut pairs = Vec::new();
        let now = Utc::now();
        for row in rows {
            let domain = row.get::<String, _>("domain");
            if !domain_matches(request_domain, &domain) {
                continue;
            }
            let path = row.get::<String, _>("path");
            if !path_matches(request_path, &path) {
                continue;
            }
            let expires_at = row.get::<Option<String>, _>("expires_at");
            if cookie_is_expired(expires_at.as_deref(), now) {
                continue;
            }
            let name = row.get::<String, _>("name");
            let value_cipher = row.get::<String, _>("value_cipher");
            let value = self.secret_box.decrypt(&value_cipher)?;
            pairs.push(CookieHeaderPair {
                path_len: path.len(),
                name: name.clone(),
                value: format!("{name}={}", value.expose_secret()),
            });
        }
        if pairs.is_empty() {
            Ok(None)
        } else {
            pairs.sort_by(|left, right| {
                right
                    .path_len
                    .cmp(&left.path_len)
                    .then_with(|| left.name.cmp(&right.name))
            });
            Ok(Some(
                pairs
                    .into_iter()
                    .map(|pair| pair.value)
                    .collect::<Vec<_>>()
                    .join("; "),
            ))
        }
    }

    /// 删除账号全部 Cookie。
    pub async fn delete_account_cookies(&self, account_id: &str) -> SqliteCookieStoreResult<u64> {
        let result = sqlx::query("delete from account_cookies where account_id = ?")
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// 删除已经过期的 Cookie。
    pub async fn cleanup_expired(&self, now: DateTime<Utc>) -> SqliteCookieStoreResult<u64> {
        let result = sqlx::query(
            "delete from account_cookies where expires_at is not null and expires_at <= ?",
        )
        .bind(now.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    async fn upsert_cookie(
        &self,
        account_id: &str,
        parsed: ParsedCookie,
    ) -> SqliteCookieStoreResult<()> {
        let now = Utc::now().to_rfc3339();
        let value_cipher = self
            .secret_box
            .encrypt(&SecretString::new(parsed.value.into()))?;
        sqlx::query(
            "insert into account_cookies (id, account_id, domain, name, value_cipher, path, expires_at, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?) on conflict(account_id, domain, name, path) do update set value_cipher = excluded.value_cipher, expires_at = excluded.expires_at, updated_at = excluded.updated_at",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(account_id)
        .bind(parsed.domain)
        .bind(parsed.name)
        .bind(value_cipher)
        .bind(parsed.path)
        .bind(parsed.expires_at)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedCookie {
    domain: String,
    name: String,
    value: String,
    path: String,
    expires_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CookieHeaderPair {
    path_len: usize,
    name: String,
    value: String,
}

fn parse_set_cookie(raw: &str) -> Option<ParsedCookie> {
    let mut parts = raw.split(';').map(str::trim);
    let (name, value) = parts.next()?.split_once('=')?;
    let name = name.trim();
    let value = value.trim();
    if name.is_empty() || value.is_empty() {
        return None;
    }

    let mut domain = DEFAULT_COOKIE_DOMAIN.to_string();
    let mut path = "/".to_string();
    let mut expires_at = None;
    for part in parts {
        let Some((attribute, value)) = part.split_once('=') else {
            continue;
        };
        match attribute.trim().to_ascii_lowercase().as_str() {
            "domain" => domain = value.trim().trim_start_matches('.').to_string(),
            "path" => path = normalize_cookie_path(value),
            "max-age" => {
                if let Ok(seconds) = value.trim().parse::<i64>() {
                    expires_at = Some(max_age_expires_at(seconds));
                }
                break;
            }
            "expires" => expires_at = Some(value.trim().to_string()),
            _ => {}
        }
    }

    Some(ParsedCookie {
        domain,
        name: name.to_string(),
        value: value.to_string(),
        path,
        expires_at,
    })
}

fn max_age_expires_at(seconds: i64) -> String {
    let now = Utc::now();
    if seconds <= 0 {
        return (now - Duration::seconds(1)).to_rfc3339();
    }
    (now + Duration::seconds(seconds.min(i32::MAX as i64))).to_rfc3339()
}

fn parse_cookie_header(raw: &str) -> Vec<ParsedCookie> {
    raw.split(';')
        .map(str::trim)
        .filter_map(|part| {
            let (name, value) = part.split_once('=')?;
            let name = name.trim();
            let value = value.trim();
            if name.is_empty() || value.is_empty() {
                return None;
            }
            Some(ParsedCookie {
                domain: DEFAULT_COOKIE_DOMAIN.to_string(),
                name: name.to_string(),
                value: value.to_string(),
                path: "/".to_string(),
                expires_at: None,
            })
        })
        .collect()
}

fn domain_matches(request_domain: &str, cookie_domain: &str) -> bool {
    request_domain == cookie_domain
        || request_domain
            .strip_suffix(cookie_domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    let request_path = normalize_request_path(request_path);
    let cookie_path = normalize_cookie_path(cookie_path);
    request_path == cookie_path
        || (request_path.starts_with(&cookie_path)
            && (cookie_path.ends_with('/')
                || request_path
                    .as_bytes()
                    .get(cookie_path.len())
                    .is_some_and(|byte| *byte == b'/')))
}

fn normalize_request_path(path: &str) -> String {
    let path = path.trim();
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

fn normalize_cookie_path(path: &str) -> String {
    let path = path.trim();
    if path.starts_with('/') {
        path.to_string()
    } else {
        "/".to_string()
    }
}

fn cookie_is_expired(expires_at: Option<&str>, now: DateTime<Utc>) -> bool {
    expires_at
        .and_then(parse_cookie_expires_at)
        .is_some_and(|expires_at| expires_at <= now)
}

fn parse_cookie_expires_at(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc2822(value)
        .or_else(|_| DateTime::parse_from_rfc3339(value))
        .map(|expires_at| expires_at.with_timezone(&Utc))
        .ok()
}
