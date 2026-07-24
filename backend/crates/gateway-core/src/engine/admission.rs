//! Client Key 准入、释放与进程启动恢复的中立契约。

use std::time::{Duration, SystemTime};

use futures::future::BoxFuture;

use crate::policy::{ClientApiKeyId, RateLimits};

use super::{ExecutionStore, ModelRequestId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientAdmissionRequest {
    pub model_request_id: ModelRequestId,
    pub client_api_key_id: ClientApiKeyId,
    pub lease_ttl: Duration,
    pub limits: RateLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAdmissionRejection {
    RateLimited,
    ConcurrencyLimited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAdmissionDecision {
    Granted,
    Rejected(ClientAdmissionRejection),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentAdmissionFact {
    pub model_request_id: ModelRequestId,
    pub started_at: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningAdmissionFact {
    pub model_request_id: ModelRequestId,
    pub expires_at: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientAdmissionRecovery {
    pub client_api_key_id: ClientApiKeyId,
    pub recent_requests: Vec<RecentAdmissionFact>,
    pub running_requests: Vec<RunningAdmissionFact>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientAdmissionRestoreResult {
    pub restored_recent_requests: u64,
    pub restored_running_requests: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("client admission store is unavailable")]
pub struct ClientAdmissionError;

pub trait ClientAdmissionPort: Send + Sync {
    fn admit(
        &self,
        request: ClientAdmissionRequest,
    ) -> BoxFuture<'_, Result<ClientAdmissionDecision, ClientAdmissionError>>;

    fn release<'a>(
        &'a self,
        client_api_key_id: &'a ClientApiKeyId,
        model_request_id: &'a ModelRequestId,
    ) -> BoxFuture<'a, Result<bool, ClientAdmissionError>>;

    fn restore(
        &self,
        recovery: ClientAdmissionRecovery,
    ) -> BoxFuture<'_, Result<ClientAdmissionRestoreResult, ClientAdmissionError>>;
}

pub trait ClientAdmissionRecoveryPort: Send + Sync {
    fn load_recovery(
        &self,
        since: SystemTime,
    ) -> BoxFuture<'_, Result<Vec<ClientAdmissionRecovery>, ClientAdmissionError>>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientAdmissionStartupRecoveryReport {
    pub expired_model_requests: u64,
    pub restored_clients: u64,
    pub restored_recent_requests: u64,
    pub restored_running_requests: u64,
}

/// 监听端口前完成 PostgreSQL 终态收敛与 Redis 热状态恢复。
pub async fn restore_client_admission_startup(
    execution: &dyn ExecutionStore,
    recovery: &dyn ClientAdmissionRecoveryPort,
    admissions: &dyn ClientAdmissionPort,
    now: SystemTime,
) -> Result<ClientAdmissionStartupRecoveryReport, ClientAdmissionError> {
    let expired = execution
        .recover_expired(now)
        .await
        .map_err(|_| ClientAdmissionError)?;
    let since = now
        .checked_sub(Duration::from_secs(61))
        .ok_or(ClientAdmissionError)?;
    let recoveries = recovery.load_recovery(since).await?;
    let restored_clients = u64::try_from(recoveries.len()).unwrap_or(u64::MAX);
    let mut restored_recent_requests = 0_u64;
    let mut restored_running_requests = 0_u64;
    for recovery in recoveries {
        let restored = admissions.restore(recovery).await?;
        restored_recent_requests =
            restored_recent_requests.saturating_add(restored.restored_recent_requests);
        restored_running_requests =
            restored_running_requests.saturating_add(restored.restored_running_requests);
    }
    Ok(ClientAdmissionStartupRecoveryReport {
        expired_model_requests: expired.requests,
        restored_clients,
        restored_recent_requests,
        restored_running_requests,
    })
}
