//! Redis 丢失后从 `model_requests` 恢复客户端准入热状态。

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_core::{
    engine::{
        ModelRequestId,
        admission::{
            ClientAdmissionError, ClientAdmissionRecovery as CoreAdmissionRecovery,
            ClientAdmissionRecoveryPort, RecentAdmissionFact, RunningAdmissionFact,
        },
    },
    policy::ClientApiKeyId,
};
use sqlx::PgPool;

use crate::{StoreError, StoreResult, postgres_unavailable};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientAdmissionRecentRequest {
    pub model_request_id: String,
    pub started_at: DateTime<Utc>,
    pub input_token_estimate: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientAdmissionRunningRequest {
    pub model_request_id: String,
    pub deadline_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientAdmissionRecovery {
    pub client_api_key_ref: String,
    pub recent_requests: Vec<ClientAdmissionRecentRequest>,
    pub running_requests: Vec<ClientAdmissionRunningRequest>,
}

#[async_trait]
pub trait ClientAdmissionRecoveryRepository: Send + Sync {
    async fn load_client_admission_recovery(
        &self,
        window_started_at: DateTime<Utc>,
    ) -> StoreResult<Vec<ClientAdmissionRecovery>>;
}

#[derive(Clone)]
pub struct PgClientAdmissionRecoveryRepository {
    pool: PgPool,
}

impl PgClientAdmissionRecoveryRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ClientAdmissionRecoveryRepository for PgClientAdmissionRecoveryRepository {
    async fn load_client_admission_recovery(
        &self,
        window_started_at: DateTime<Utc>,
    ) -> StoreResult<Vec<ClientAdmissionRecovery>> {
        let rows =
            sqlx::query_as::<_, (String, String, i64, DateTime<Utc>, DateTime<Utc>, String)>(
                "select client_api_key_ref, id, input_token_estimate, started_at,
                    deadline_at, outcome
             from model_requests
             where started_at >= $1 or outcome = 'running'
             order by client_api_key_ref, started_at, id",
            )
            .bind(window_started_at)
            .fetch_all(&self.pool)
            .await
            .map_err(|_| postgres_unavailable("load client admission recovery"))?;
        let mut recoveries = BTreeMap::<String, ClientAdmissionRecovery>::new();
        for (
            client_api_key_ref,
            model_request_id,
            input_token_estimate,
            started_at,
            deadline_at,
            outcome,
        ) in rows
        {
            let recovery = recoveries
                .entry(client_api_key_ref.clone())
                .or_insert_with(|| ClientAdmissionRecovery {
                    client_api_key_ref,
                    recent_requests: Vec::new(),
                    running_requests: Vec::new(),
                });
            if started_at >= window_started_at {
                recovery.recent_requests.push(ClientAdmissionRecentRequest {
                    model_request_id: model_request_id.clone(),
                    started_at,
                    input_token_estimate: to_u64(input_token_estimate)?,
                });
            }
            if outcome == "running" {
                recovery
                    .running_requests
                    .push(ClientAdmissionRunningRequest {
                        model_request_id,
                        deadline_at,
                    });
            }
        }
        Ok(recoveries.into_values().collect())
    }
}

impl ClientAdmissionRecoveryPort for PgClientAdmissionRecoveryRepository {
    fn load_recovery(
        &self,
        since: std::time::SystemTime,
    ) -> futures::future::BoxFuture<'_, Result<Vec<CoreAdmissionRecovery>, ClientAdmissionError>>
    {
        Box::pin(async move {
            self.load_client_admission_recovery(DateTime::<Utc>::from(since))
                .await
                .map_err(|_| ClientAdmissionError)?
                .into_iter()
                .map(|recovery| {
                    let client_api_key_id = ClientApiKeyId::new(recovery.client_api_key_ref)
                        .map_err(|_| ClientAdmissionError)?;
                    let recent_requests = recovery
                        .recent_requests
                        .into_iter()
                        .map(|request| {
                            Ok(RecentAdmissionFact {
                                model_request_id: ModelRequestId::new(request.model_request_id)
                                    .map_err(|_| ClientAdmissionError)?,
                                started_at: request.started_at.into(),
                                input_token_estimate: request.input_token_estimate,
                            })
                        })
                        .collect::<Result<Vec<_>, ClientAdmissionError>>()?;
                    let running_requests = recovery
                        .running_requests
                        .into_iter()
                        .map(|request| {
                            Ok(RunningAdmissionFact {
                                model_request_id: ModelRequestId::new(request.model_request_id)
                                    .map_err(|_| ClientAdmissionError)?,
                                expires_at: request.deadline_at.into(),
                            })
                        })
                        .collect::<Result<Vec<_>, ClientAdmissionError>>()?;
                    Ok(CoreAdmissionRecovery {
                        client_api_key_id,
                        recent_requests,
                        running_requests,
                    })
                })
                .collect()
        })
    }
}

fn to_u64(value: i64) -> StoreResult<u64> {
    u64::try_from(value).map_err(|_| StoreError::InvalidData {
        entity: "client admission recovery",
        message: "input token estimate is negative".to_owned(),
    })
}
