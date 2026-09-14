//! 可丢失、可从 PostgreSQL 或 Provider 重建的 Redis 协调状态。

use sha2::{Digest, Sha256};

mod admin_account_runtime;
mod artifact_profile;
mod auth;
mod client_admission;
mod coordination_buffer;
mod credential_cooldown;
mod credential_leases;
mod credential_state;
mod native_continuation;
mod oauth_pending;
mod provider_circuit;
mod provider_session_affinity;
mod provider_session_exclusion;
mod runtime_change;
pub(crate) mod worker_lease;

pub use admin_account_runtime::*;
pub use artifact_profile::*;
pub use auth::*;
pub use client_admission::*;
pub use coordination_buffer::*;
pub use credential_cooldown::*;
pub use credential_leases::*;
pub use credential_state::*;
pub use native_continuation::*;
pub use oauth_pending::*;
pub use provider_circuit::*;
pub use provider_session_affinity::*;
pub use provider_session_exclusion::*;
pub use runtime_change::*;

use crate::{StoreError, StoreResult, require_nonempty};

pub(crate) const MAX_REDIS_EXACT_INTEGER: u64 = (1_u64 << 53) - 1;

pub(crate) fn resource_fingerprint(entity: &'static str, value: &str) -> StoreResult<String> {
    require_nonempty(entity, "resource ID", value)?;
    Ok(hex::encode(Sha256::digest(value.as_bytes())))
}

pub(crate) fn namespace(value: &str) -> StoreResult<String> {
    require_nonempty("Redis namespace", "namespace", value)?;
    if value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(StoreError::InvalidData {
            entity: "Redis namespace",
            message: "namespace contains unsupported characters".to_owned(),
        });
    }
    Ok(value.to_owned())
}
