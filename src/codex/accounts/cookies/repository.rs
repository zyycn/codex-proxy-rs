use chrono::{DateTime, Duration, Utc};
use secrecy::{ExposeSecret, SecretString};
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use crate::platform::crypto::{CryptoError, SecretBox};

const CAPTURABLE_COOKIES: &[&str] = &["cf_clearance"];

#[derive(Debug, Error)]
pub enum CookieError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("cookie encryption error: {0}")]
    Crypto(#[from] CryptoError),
}

pub type CookieResult<T> = Result<T, CookieError>;

#[derive(Clone)]
pub struct CookieRepository {
    pool: SqlitePool,
    secret_box: SecretBox,
}

impl CookieRepository {
    pub fn new(pool: SqlitePool, secret_box: SecretBox) -> Self {
        Self { pool, secret_box }
    }

    pub async fn capture_set_cookie(&self, account_id: &str, raw: &str) -> CookieResult<()> {
        let Some(parsed) = parse_set_cookie(raw) else {
            return Ok(());
        };
        if !CAPTURABLE_COOKIES.contains(&parsed.name.as_str()) {
            return Ok(());
        }
        self.upsert_cookie(account_id, parsed).await
    }

    pub async fn set_cookie_header(&self, account_id: &str, raw: &str) -> CookieResult<usize> {
        let parsed = parse_cookie_header(raw);
        let count = parsed.len();
        for cookie in parsed {
            self.upsert_cookie(account_id, cookie).await?;
        }
        Ok(count)
    }

    async fn upsert_cookie(&self, account_id: &str, parsed: ParsedCookie) -> CookieResult<()> {
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

    pub async fn cookie_header(
        &self,
        account_id: &str,
        request_domain: &str,
    ) -> CookieResult<Option<String>> {
        let rows = sqlx::query(
            "select domain, name, value_cipher, expires_at from account_cookies where account_id = ? order by name asc",
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
            if cookie_is_expired(row.get::<Option<String>, _>("expires_at").as_deref(), now) {
                continue;
            }
            let name = row.get::<String, _>("name");
            let value_cipher = row.get::<String, _>("value_cipher");
            let value = self.secret_box.decrypt(&value_cipher)?;
            pairs.push(format!("{name}={}", value.expose_secret()));
        }
        if pairs.is_empty() {
            Ok(None)
        } else {
            Ok(Some(pairs.join("; ")))
        }
    }

    pub async fn delete_account_cookies(&self, account_id: &str) -> CookieResult<u64> {
        let result = sqlx::query("delete from account_cookies where account_id = ?")
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    pub async fn cleanup_expired(&self, now: DateTime<Utc>) -> CookieResult<u64> {
        let rows =
            sqlx::query("select id, expires_at from account_cookies where expires_at is not null")
                .fetch_all(&self.pool)
                .await?;
        let mut deleted = 0;
        for row in rows {
            let expires_at = row.get::<String, _>("expires_at");
            if !cookie_is_expired(Some(&expires_at), now) {
                continue;
            }
            let result = sqlx::query("delete from account_cookies where id = ?")
                .bind(row.get::<String, _>("id"))
                .execute(&self.pool)
                .await?;
            deleted += result.rows_affected();
        }
        Ok(deleted)
    }
}

struct ParsedCookie {
    domain: String,
    name: String,
    value: String,
    path: String,
    expires_at: Option<String>,
}

fn parse_set_cookie(raw: &str) -> Option<ParsedCookie> {
    let mut parts = raw.split(';').map(str::trim);
    let (name, value) = parts.next()?.split_once('=')?;
    let mut domain = "chatgpt.com".to_string();
    let mut path = "/".to_string();
    let mut expires_at = None;
    for part in parts {
        let Some((attribute, value)) = part.split_once('=') else {
            continue;
        };
        match attribute.trim().to_ascii_lowercase().as_str() {
            "domain" => domain = value.trim_start_matches('.').to_string(),
            "path" => path = value.to_string(),
            "max-age" => {
                if let Ok(seconds) = value.parse::<i64>() {
                    expires_at = Some(max_age_expires_at(seconds));
                }
                break;
            }
            "expires" => expires_at = Some(value.to_string()),
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
                domain: "chatgpt.com".to_string(),
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
