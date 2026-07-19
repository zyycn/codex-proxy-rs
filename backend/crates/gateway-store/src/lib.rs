//! 多 Provider 网关的 PostgreSQL 持久化与 Redis 协调 adapter。
//!
//! 业务规则与 port 由 `gateway-core` 拥有。本 crate 只负责把终态的十张业务表
//! 和可丢失 Redis 状态映射为明确的基础设施操作。

#![forbid(unsafe_code)]

use std::{fmt, num::NonZeroU64, str::FromStr};

use serde_json::{Map, Value};

pub mod postgres;
pub mod redis;

/// 保持依赖方向为 `gateway-store -> gateway-core`。
pub use gateway_core as core;

/// 发生错误的基础设施边界。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreBackend {
    PostgreSql,
    Redis,
}

/// 上层状态机需要区分的稳定冲突类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    StaleRevision,
    AlreadyFinalized,
    DownstreamAlreadyCommitted,
    RequestNotRunning,
    InvalidTransition,
    LeaseLost,
    FencingTokenStale,
}

/// Store adapter 的稳定错误边界。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("{backend:?} store is unavailable: {message}")]
    Unavailable {
        backend: StoreBackend,
        message: String,
    },
    #[error("{entity} {id} was not found")]
    NotFound { entity: &'static str, id: String },
    #[error("store conflict for {entity} {id}: {kind:?}")]
    Conflict {
        entity: &'static str,
        id: String,
        kind: ConflictKind,
    },
    #[error("invalid persisted {entity}: {message}")]
    InvalidData {
        entity: &'static str,
        message: String,
    },
}

pub type StoreResult<T> = Result<T, StoreError>;

/// 正整数 revision 或 fencing token。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(NonZeroU64);

impl Revision {
    pub fn new(value: u64) -> StoreResult<Self> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or_else(|| StoreError::InvalidData {
                entity: "revision",
                message: "must be greater than zero".to_owned(),
            })
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// `numeric(20,10)` 可无损表达的非负金额。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DecimalAmount(String);

impl DecimalAmount {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DecimalAmount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for DecimalAmount {
    type Err = StoreError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        let mut parts = input.split('.');
        let whole = parts.next().unwrap_or_default();
        let fraction = parts.next();
        let valid = !whole.is_empty()
            && whole.len() <= 10
            && whole.bytes().all(|byte| byte.is_ascii_digit())
            && parts.next().is_none()
            && fraction.is_none_or(|value| {
                !value.is_empty()
                    && value.len() <= 10
                    && value.bytes().all(|byte| byte.is_ascii_digit())
            });
        if !valid {
            return Err(StoreError::InvalidData {
                entity: "decimal amount",
                message: "expected a non-negative numeric(20,10) value".to_owned(),
            });
        }

        let whole = whole.trim_start_matches('0');
        let whole = if whole.is_empty() { "0" } else { whole };
        let fraction = fraction.unwrap_or_default().trim_end_matches('0');
        let canonical = if fraction.is_empty() {
            whole.to_owned()
        } else {
            format!("{whole}.{fraction}")
        };
        Ok(Self(canonical))
    }
}

/// Provider-owned JSON object。Store 只验证 object 与大小，不解释内部 key。
#[derive(Clone, PartialEq)]
pub struct JsonObject(Map<String, Value>);

impl JsonObject {
    pub fn try_from_value(
        entity: &'static str,
        value: Value,
        max_serialized_bytes: usize,
    ) -> StoreResult<Self> {
        let serialized_bytes = serde_json::to_vec(&value)
            .map_err(|error| StoreError::InvalidData {
                entity,
                message: error.to_string(),
            })?
            .len();
        let Value::Object(fields) = value else {
            return Err(StoreError::InvalidData {
                entity,
                message: "top-level JSON value must be an object".to_owned(),
            });
        };
        if serialized_bytes > max_serialized_bytes {
            return Err(StoreError::InvalidData {
                entity,
                message: format!("serialized JSON exceeds {max_serialized_bytes} bytes"),
            });
        }
        Ok(Self(fields))
    }

    #[must_use]
    pub fn as_value(&self) -> Value {
        Value::Object(self.0.clone())
    }

    #[must_use]
    pub fn fields(&self) -> &Map<String, Value> {
        &self.0
    }
}

impl fmt::Debug for JsonObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JsonObject([REDACTED])")
    }
}

pub(crate) fn require_nonempty(
    entity: &'static str,
    field: &'static str,
    value: &str,
) -> StoreResult<()> {
    if value.trim().is_empty() {
        Err(StoreError::InvalidData {
            entity,
            message: format!("{field} must not be empty"),
        })
    } else {
        Ok(())
    }
}

pub(crate) fn postgres_unavailable(operation: &'static str) -> StoreError {
    StoreError::Unavailable {
        backend: StoreBackend::PostgreSql,
        message: operation.to_owned(),
    }
}

pub(crate) fn redis_unavailable(operation: &'static str) -> StoreError {
    StoreError::Unavailable {
        backend: StoreBackend::Redis,
        message: operation.to_owned(),
    }
}
