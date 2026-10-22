//! 与原生调度状态分离的上游额度展示快照及刷新写入

use std::time::{Duration, SystemTime};

use super::{CredentialRevision, OpaqueProviderData, ProviderAccountId};

#[derive(Debug, Clone)]
pub struct UpstreamQuotaSnapshot {
    pub account_id: ProviderAccountId,
    pub credential_revision: CredentialRevision,
    pub fetched_at: Option<SystemTime>,
    pub last_attempt_at: Option<SystemTime>,
    pub last_error_code: Option<String>,
    pub forwarding_blocked: bool,
    pub payload: Option<OpaqueProviderData>,
}

pub struct UpstreamQuotaRefreshClaim {
    pub account_id: ProviderAccountId,
    pub credential_revision: CredentialRevision,
    pub attempt_id: String,
    pub attempted_at: SystemTime,
    pub minimum_interval: Duration,
}

pub struct UpstreamQuotaRefreshResult {
    pub claim: UpstreamQuotaRefreshClaim,
    pub payload: Option<OpaqueProviderData>,
    pub error_code: Option<String>,
}
