//! 刷新租约存储、JWT 解码与账号 claims 验证。

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Map, Value};

use crate::accounts::oauth::{RefreshFailure, TokenPair};

// ---------------------------------------------------------------------------
// JWT 解码
// ---------------------------------------------------------------------------

/// JWT 过期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JwtExpiry {
    /// token 已过期。
    Expired,
    /// token 仍然有效。
    Valid,
    /// token 缺失、格式错误或不包含可解析的 exp。
    MissingOrInvalid,
}

/// 按给定时间点判断 JWT 的 `exp` 是否已过期。
pub fn jwt_expiry(token: &str, now: DateTime<Utc>) -> JwtExpiry {
    let Some(exp) = jwt_exp(token) else {
        return JwtExpiry::MissingOrInvalid;
    };
    if now.timestamp() >= exp {
        JwtExpiry::Expired
    } else {
        JwtExpiry::Valid
    }
}

/// 读取 JWT `exp` 并转换成 UTC 时间。
pub fn jwt_expiration(token: &str) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt(jwt_exp(token)?, 0).single()
}

fn jwt_exp(token: &str) -> Option<i64> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value = serde_json::from_slice::<Value>(&decoded).ok()?;
    value.get("exp")?.as_i64()
}

// ---------------------------------------------------------------------------
// 账号 claims 解码（手动创建/导入时验证 JWT）
// ---------------------------------------------------------------------------

/// 手动创建账号时从 JWT 提取的 claims。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualAccountClaims {
    /// ChatGPT 账号 ID。
    pub account_id: Option<String>,
    /// ChatGPT 用户 ID。
    pub user_id: Option<String>,
    /// 邮箱。
    pub email: Option<String>,
    /// 订阅计划类型。
    pub plan_type: Option<String>,
    /// access token 过期时间。
    pub expires_at: DateTime<Utc>,
}

/// 从 JWT 中解码 payload 部分。
pub fn decode_jwt_payload(token: &str) -> Option<Map<String, Value>> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    if payload.is_empty() {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice::<Value>(&bytes)
        .ok()?
        .as_object()
        .cloned()
}

/// 从 JWT 中提取手动创建账号所需的 claims。
pub fn manual_account_claims(
    token: &str,
    now: DateTime<Utc>,
) -> Result<ManualAccountClaims, &'static str> {
    let payload = decode_jwt_payload(token).ok_or("Invalid JWT format")?;
    let exp = payload
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or("Token is expired")?;
    if now.timestamp() >= exp {
        return Err("Token is expired");
    }
    let expires_at = DateTime::<Utc>::from_timestamp(exp, 0).ok_or("Invalid JWT exp claim")?;
    let auth = payload
        .get("https://api.openai.com/auth")
        .and_then(Value::as_object)
        .ok_or("Token missing OpenAI auth claim")?;
    let account_id = string_claim(auth, "chatgpt_account_id");
    let profile = payload
        .get("https://api.openai.com/profile")
        .and_then(Value::as_object);
    let user_id = string_claim(auth, "chatgpt_user_id")
        .or_else(|| profile.and_then(|profile| string_claim(profile, "chatgpt_user_id")))
        .or_else(|| string_claim(auth, "user_id"))
        .or_else(|| profile.and_then(|profile| string_claim(profile, "user_id")));
    let plan_type = string_claim(auth, "chatgpt_plan_type")
        .or_else(|| profile.and_then(|profile| string_claim(profile, "chatgpt_plan_type")));
    let email = profile.and_then(|profile| string_claim(profile, "email"));

    Ok(ManualAccountClaims {
        account_id,
        user_id,
        email,
        plan_type,
        expires_at,
    })
}

fn string_claim(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

// ---------------------------------------------------------------------------
// TokenRefresher 端口
// ---------------------------------------------------------------------------

/// 刷新令牌的上游端口。
#[async_trait]
pub trait TokenRefresher: Send + Sync + 'static {
    /// 使用给定刷新令牌换取新的 token 对。
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure>;
}

// ---------------------------------------------------------------------------
// 刷新租约存储（SQLite）
// ---------------------------------------------------------------------------

use sqlx::SqlitePool;
use thiserror::Error;

/// SQLite 刷新租约存储结果。
pub type RefreshLeaseStoreResult<T> = Result<T, RefreshLeaseStoreError>;

/// SQLite 刷新租约存储错误。
#[derive(Debug, Error)]
pub enum RefreshLeaseStoreError {
    /// SQLite 操作失败。
    #[error("sqlite refresh lease query failed: {0}")]
    Sqlx(#[from] sqlx::Error),
}

/// SQLite 刷新租约存储。
#[derive(Clone)]
pub struct RefreshLeaseStore {
    pool: SqlitePool,
}

impl RefreshLeaseStore {
    /// 构造存储。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 返回底层连接池。
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// 尝试获取账号刷新租约。
    pub async fn try_acquire(
        &self,
        account_id: &str,
        owner: &str,
        expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> RefreshLeaseStoreResult<bool> {
        let result = sqlx::query(
            r#"
insert into account_refresh_leases (account_id, owner, expires_at, updated_at)
values (?, ?, ?, ?)
on conflict(account_id) do update set
  owner = excluded.owner,
  expires_at = excluded.expires_at,
  updated_at = excluded.updated_at
where account_refresh_leases.expires_at <= ?
  or account_refresh_leases.owner = ?
"#,
        )
        .bind(account_id)
        .bind(owner)
        .bind(expires_at.to_rfc3339())
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .bind(owner)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 释放账号刷新租约。
    pub async fn release(&self, account_id: &str, owner: &str) -> RefreshLeaseStoreResult<bool> {
        let result =
            sqlx::query("delete from account_refresh_leases where account_id = ? and owner = ?")
                .bind(account_id)
                .bind(owner)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected() > 0)
    }
}
