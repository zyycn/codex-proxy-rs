//! Store 值类型、错误与跨层映射。

use super::*;

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
pub(crate) fn store_revision(revision: AdminRevision) -> AdminStoreResult<Revision> {
    Revision::new(revision.get()).map_err(|error| admin_store_error("config revision", error))
}

pub(crate) fn admin_revision(revision: Revision) -> AdminStoreResult<AdminRevision> {
    AdminRevision::new(revision.get()).map_err(|_| {
        AdminStoreError::new(
            AdminStoreErrorKind::Invalid,
            "config revision",
            "config revision is invalid",
        )
    })
}

pub(crate) fn mutation_audit(
    context: &MutationContext,
    action: &str,
    entity_kind: &str,
    entity_ref: &str,
    changed_fields: Vec<String>,
) -> postgres::AdminAuditEvent {
    let (actor_kind, actor_admin_user_id, actor_ref) = match &context.actor {
        MutationActor::AdminSession { admin_user_id } => (
            postgres::AdminAuditActorKind::AdminSession,
            Some(admin_user_id.clone()),
            admin_user_id.clone(),
        ),
        MutationActor::AdminApiKey => (
            postgres::AdminAuditActorKind::AdminApiKey,
            None,
            "admin_api_key".to_owned(),
        ),
        MutationActor::System => (
            postgres::AdminAuditActorKind::System,
            None,
            "system".to_owned(),
        ),
    };
    postgres::AdminAuditEvent {
        id: format!("audit_{}", uuid::Uuid::now_v7().simple()),
        actor_kind,
        actor_admin_user_id,
        actor_ref,
        admin_request_id: Some(context.request_id.clone()),
        action: action.to_owned(),
        entity_kind: entity_kind.to_owned(),
        entity_ref: entity_ref.to_owned(),
        config_revision: None,
        changed_fields,
        created_at: chrono::Utc::now(),
    }
}

pub(crate) fn admin_store_error(resource: &'static str, error: StoreError) -> AdminStoreError {
    let kind = match error {
        StoreError::NotFound { .. } => AdminStoreErrorKind::NotFound,
        StoreError::Conflict {
            kind: ConflictKind::StaleRevision,
            ..
        } => AdminStoreErrorKind::StaleRevision,
        StoreError::Conflict { .. } => AdminStoreErrorKind::Conflict,
        StoreError::InvalidData { .. } => AdminStoreErrorKind::Invalid,
        StoreError::Unavailable { .. } => AdminStoreErrorKind::Unavailable,
    };
    AdminStoreError::new(kind, resource, "store operation failed")
}

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
