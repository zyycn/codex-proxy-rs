//! Provider credential 值对象构造错误。

use thiserror::Error;

/// Credential 值对象构造错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CredentialError {
    #[error("credential revision must be greater than zero")]
    InvalidRevision,
    #[error("credential revision overflow")]
    RevisionOverflow,
    #[error("credential CAS profile belongs to a different account")]
    ProfileAccountMismatch,
    #[error("credential refresh schedule requires a refresh token")]
    InvalidRefreshSchedule,
}
