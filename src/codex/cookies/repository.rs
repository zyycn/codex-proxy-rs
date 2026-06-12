use chrono::Utc;
use secrecy::{ExposeSecret, SecretString};
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use uuid::Uuid;

use crate::utils::crypto::{CryptoError, SecretBox};

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
            "select domain, name, value_cipher from account_cookies where account_id = ? order by name asc",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;
        let mut pairs = Vec::new();
        for row in rows {
            let domain = row.get::<String, _>("domain");
            if !domain_matches(request_domain, &domain) {
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
        if let Some(value) = part.strip_prefix("Domain=") {
            domain = value.trim_start_matches('.').to_string();
        } else if let Some(value) = part.strip_prefix("Path=") {
            path = value.to_string();
        } else if let Some(value) = part.strip_prefix("Expires=") {
            expires_at = Some(value.to_string());
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
