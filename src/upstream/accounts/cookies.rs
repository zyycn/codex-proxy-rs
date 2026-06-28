//! Cookie 捕获、重放与 Cloudflare 风控策略。

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use tokio::sync::RwLock;
use uuid::Uuid;

/// 单个 Cookie 条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookieEntry {
    /// Cookie 名称。
    pub name: String,
    /// Cookie 值。
    pub value: String,
}

/// 将 Cookie 条目组装为 HTTP Cookie 头。
pub fn cookie_header(cookies: &[CookieEntry]) -> Option<String> {
    let header = cookies
        .iter()
        .filter(|cookie| !cookie.name.trim().is_empty())
        .map(|cookie| format!("{}={}", cookie.name.trim(), cookie.value))
        .collect::<Vec<_>>()
        .join("; ");
    (!header.is_empty()).then_some(header)
}

const DEFAULT_COOKIE_DOMAIN: &str = "chatgpt.com";
const CAPTURABLE_COOKIES: &[&str] = &["cf_clearance"];

/// SQLite Cookie 存储错误。
#[derive(Debug, Error)]
pub enum SqliteCookieStoreError {
    /// 数据库错误。
    #[error("sqlite cookie store database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// SQLite Cookie 存储结果。
pub type SqliteCookieStoreResult<T> = Result<T, SqliteCookieStoreError>;

/// SQLite Cookie 存储。
#[derive(Clone)]
pub struct SqliteCookieStore {
    pool: SqlitePool,
}

impl SqliteCookieStore {
    /// 构造 Cookie 存储。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
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
            "select domain, name, value, path, expires_at from account_cookies where account_id = ?",
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
            pairs.push(CookieHeaderPair {
                path_len: path.len(),
                name: name.clone(),
                value: format!("{name}={}", row.get::<String, _>("value")),
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

    /// 删除指定时间之前过期的 Cookie。
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
        sqlx::query(
            "insert into account_cookies (id, account_id, domain, name, value, path, expires_at, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?) on conflict(account_id, domain, name, path) do update set value = excluded.value, expires_at = excluded.expires_at, updated_at = excluded.updated_at",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(account_id)
        .bind(parsed.domain)
        .bind(parsed.name)
        .bind(parsed.value)
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

const PATH_BLOCK_THRESHOLD: u32 = 3;
const PATH_BLOCK_STALE_AFTER: Duration = Duration::hours(1);
const CHALLENGE_BACKOFF_SECONDS: [i64; 4] = [10, 30, 90, 120];
const CHALLENGE_STALE_AFTER: Duration = Duration::hours(1);

#[derive(Debug, Clone, Copy)]
struct PathBlockState {
    count: u32,
    last_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy)]
struct ChallengeCooldownState {
    challenge_count: u32,
    updated_at: DateTime<Utc>,
}

/// 记录一次 Cloudflare challenge 后得到的冷却状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloudflareChallengeCooldown {
    /// 当前未过期的连续 challenge 次数。
    pub challenge_count: u32,
    /// 本次 challenge 选择的退避秒数。
    pub delay_seconds: i64,
    /// 账号应跳过到这个时间点。
    pub cooldown_until: DateTime<Utc>,
    /// 本次 challenge 的记录时间。
    pub updated_at: DateTime<Utc>,
}

/// 跟踪账号维度的 Cloudflare path-block 失败。
#[derive(Debug, Clone, Default)]
pub struct CloudflarePathBlockTracker {
    counts: Arc<RwLock<HashMap<String, PathBlockState>>>,
}

impl CloudflarePathBlockTracker {
    /// 构造空 path-block tracker。
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次 path-block 失败，并返回当前未过期次数。
    pub async fn record_path_block(&self, account_id: &str, now: DateTime<Utc>) -> u32 {
        let mut counts = self.counts.write().await;
        let count = counts
            .get(account_id)
            .filter(|state| now.signed_duration_since(state.last_at) <= PATH_BLOCK_STALE_AFTER)
            .map_or(1, |state| state.count.saturating_add(1));
        counts.insert(
            account_id.to_string(),
            PathBlockState {
                count,
                last_at: now,
            },
        );
        count
    }

    /// 清理指定账号的 path-block 失败记录。
    pub async fn reset(&self, account_id: &str) {
        self.counts.write().await.remove(account_id);
    }

    /// 返回指定账号当前未过期的 path-block 次数。
    pub async fn count(&self, account_id: &str, now: DateTime<Utc>) -> u32 {
        self.counts
            .read()
            .await
            .get(account_id)
            .filter(|state| now.signed_duration_since(state.last_at) <= PATH_BLOCK_STALE_AFTER)
            .map(|state| state.count)
            .unwrap_or_default()
    }

    /// 判断当前次数是否已达到禁用账号阈值。
    pub async fn should_disable(&self, account_id: &str, now: DateTime<Utc>) -> bool {
        self.count(account_id, now).await >= PATH_BLOCK_THRESHOLD
    }
}

/// 跟踪账号维度的 Cloudflare challenge 冷却升级。
#[derive(Debug, Clone, Default)]
pub struct CloudflareChallengeCooldownTracker {
    states: Arc<RwLock<HashMap<String, ChallengeCooldownState>>>,
}

impl CloudflareChallengeCooldownTracker {
    /// 构造空 challenge cooldown tracker。
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次 challenge，并返回当前未过期冷却状态。
    pub async fn record_challenge(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
    ) -> CloudflareChallengeCooldown {
        let mut states = self.states.write().await;
        let challenge_count = states
            .get(account_id)
            .filter(|state| now.signed_duration_since(state.updated_at) <= CHALLENGE_STALE_AFTER)
            .map_or(1, |state| state.challenge_count.saturating_add(1));
        states.insert(
            account_id.to_string(),
            ChallengeCooldownState {
                challenge_count,
                updated_at: now,
            },
        );
        let delay_seconds = challenge_delay_seconds(challenge_count);
        CloudflareChallengeCooldown {
            challenge_count,
            delay_seconds,
            cooldown_until: now + Duration::seconds(delay_seconds),
            updated_at: now,
        }
    }

    /// 清理指定账号的 challenge 冷却状态。
    pub async fn reset(&self, account_id: &str) {
        self.states.write().await.remove(account_id);
    }
}

fn challenge_delay_seconds(challenge_count: u32) -> i64 {
    let index = challenge_count
        .saturating_sub(1)
        .min((CHALLENGE_BACKOFF_SECONDS.len() - 1) as u32) as usize;
    CHALLENGE_BACKOFF_SECONDS[index]
}
