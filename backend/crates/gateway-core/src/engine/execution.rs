//! 数据面执行用例：认证、准入、路由、continuation、circuit 与会话生命周期。

use std::collections::BTreeSet;
use std::fmt;
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::{FutureExt, future::BoxFuture, pin_mut, select_biased};
use futures_timer::Delay;
use uuid::Uuid;

use crate::engine::admission::{
    ClientAdmissionDecision, ClientAdmissionPort, ClientAdmissionRejection, ClientAdmissionRequest,
};
use crate::engine::continuation::{
    ContinuationBinding, NativeContinuationPin, NativeContinuationPort, PreviousResponseId,
};
use crate::engine::coordinator::ResponseExecutionSession;
use crate::engine::probe::{
    AccountProbe, AccountProbeError, AccountProbeRequest, AccountProbeResult,
    AccountProbeUpstreamResponse,
};
use crate::engine::provider::ProviderRegistry;
use crate::engine::{
    AttemptCoordinator, AttemptRecord, CancellationToken, CoordinatedEvent, EngineError,
    ExecutionStore, GatewayEngine, IntermediateFailure, ModelRequestFinalization, ModelRequestId,
    NewModelRequest, ProbeFailure, ProviderAccountId, ProviderAttemptOutcome, RecoveryReport,
    UpstreamSendState,
};
use crate::error::{GatewayError, GatewayErrorKind, ProviderErrorKind, StoreError};
use crate::event::{GatewayEvent, ProviderEvent, ProviderResponseHeader};
use crate::operation::{Operation, ProviderSessionState};
use crate::policy::{ClientApiKeyId, ClientPolicy};
use crate::routing::snapshot::RuntimeSnapshotHandle;
use crate::routing::{
    ProviderKind, PublicModelId, PublicModelProfile, RoutingContext, RuntimeSnapshot,
    UpstreamModelId,
};

const MODEL_REQUEST_DEADLINE: Duration = Duration::from_secs(10 * 60);
const COORDINATION_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientTransport {
    HttpJson,
    HttpSse,
    WebSocket,
    InternalProbe,
}

impl ClientTransport {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HttpJson => "http_json",
            Self::HttpSse => "http_sse",
            Self::WebSocket => "websocket",
            Self::InternalProbe => "internal",
        }
    }
}

/// API 解码后交给 Core 的稳定请求元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRequestMetadata {
    pub protocol: String,
    pub endpoint: String,
    pub transport: ClientTransport,
    pub stream: bool,
    pub client_ip: Option<IpAddr>,
    pub user_agent: Option<String>,
    pub previous_response_id: Option<PreviousResponseId>,
}

#[derive(Clone)]
pub struct AuthenticatedClient {
    snapshot: Arc<RuntimeSnapshot>,
    policy: ClientPolicy,
}

impl AuthenticatedClient {
    #[must_use]
    pub const fn snapshot(&self) -> &Arc<RuntimeSnapshot> {
        &self.snapshot
    }

    #[must_use]
    pub const fn policy(&self) -> &ClientPolicy {
        &self.policy
    }
}

impl fmt::Debug for AuthenticatedClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedClient")
            .field("key_id", &self.policy.key_id())
            .field("revision", &self.snapshot.revision())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ClientAuthenticationError {
    #[error("client API key is invalid")]
    InvalidKey,
    #[error("runtime snapshot is unavailable")]
    SnapshotUnavailable,
}

pub struct StartExecution {
    pub client: AuthenticatedClient,
    pub public_model: PublicModelId,
    pub operation: Operation,
    pub metadata: ExecutionRequestMetadata,
}

pub struct StartedExecution {
    pub request_id: ModelRequestId,
    pub created_at: SystemTime,
    pub stream: bool,
    pub session: Box<dyn ExecutionSession>,
}

pub trait ExecutionSession: Send {
    fn next_event(&mut self) -> BoxFuture<'_, Result<Option<CoordinatedEvent>, EngineError>>;
    fn collect_uncommitted(&mut self) -> BoxFuture<'_, Result<Vec<ProviderEvent>, EngineError>>;
    fn response_headers(&self) -> &[ProviderResponseHeader];
    fn commit_downstream(
        &mut self,
        client_status_code: Option<u16>,
    ) -> BoxFuture<'_, Result<(), EngineError>>;
    fn record_client_status(
        &mut self,
        client_status_code: u16,
    ) -> BoxFuture<'_, Result<(), EngineError>>;
    fn is_finalized(&self) -> bool;
    fn cancel(&self);
    fn detach_finalize(self: Box<Self>) -> BoxFuture<'static, ()>;
}

pub trait ExecutionService: Send + Sync {
    fn authenticate(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, ClientAuthenticationError>;
    fn public_models(&self, client: &AuthenticatedClient) -> Vec<PublicModelId>;
    fn public_model_profiles(&self, _client: &AuthenticatedClient) -> Vec<PublicModelProfile> {
        Vec::new()
    }
    fn contains_public_model(&self, client: &AuthenticatedClient, model: &PublicModelId) -> bool;
    fn start(
        &self,
        request: StartExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>>;
}

/// 成功认证后的 API Key 使用事实接收器。
///
/// 认证仍是同步快照读取；实现必须自行异步、去重地持久化，不得阻塞客户端请求。
pub trait ClientApiKeyUsageSink: Send + Sync {
    fn record_used(&self, key_id: &ClientApiKeyId);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCircuitDecision {
    Allow,
    BlockedUntil(SystemTime),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("provider circuit store is unavailable")]
pub struct ProviderCircuitError;

/// Provider circuit 的可重建协调策略；由 Core 拥有并交给 Store adapter 执行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCircuitPolicy {
    pub failure_threshold: NonZeroU32,
    pub open_duration: Duration,
}

impl Default for ProviderCircuitPolicy {
    fn default() -> Self {
        Self {
            failure_threshold: NonZeroU32::new(3).unwrap_or(NonZeroU32::MIN),
            open_duration: Duration::from_secs(30),
        }
    }
}

pub trait ProviderCircuitPort: Send + Sync {
    fn decision<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
    ) -> BoxFuture<'a, Result<ProviderCircuitDecision, ProviderCircuitError>>;
    fn observe_failure<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
    ) -> BoxFuture<'a, Result<(), ProviderCircuitError>>;
    fn observe_success<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
    ) -> BoxFuture<'a, Result<(), ProviderCircuitError>>;
}

pub struct DefaultExecutionService {
    snapshots: RuntimeSnapshotHandle,
    coordinator: Arc<AttemptCoordinator<dyn ExecutionStore>>,
    probe_coordinator: Arc<AttemptCoordinator<dyn ExecutionStore>>,
    /// probe 自身走 transient store，探测失败仍写入持久 store 的 ops_events。
    observations: Arc<dyn ExecutionStore>,
    providers: ProviderRegistry,
    admissions: Arc<dyn ClientAdmissionPort>,
    circuits: Arc<dyn ProviderCircuitPort>,
    continuation: Arc<dyn NativeContinuationPort>,
    client_api_key_usage: Arc<dyn ClientApiKeyUsageSink>,
}

impl DefaultExecutionService {
    #[must_use]
    pub fn new(
        snapshots: RuntimeSnapshotHandle,
        execution: Arc<dyn ExecutionStore>,
        providers: ProviderRegistry,
        admissions: Arc<dyn ClientAdmissionPort>,
        circuits: Arc<dyn ProviderCircuitPort>,
        continuation: Arc<dyn NativeContinuationPort>,
        client_api_key_usage: Arc<dyn ClientApiKeyUsageSink>,
    ) -> Self {
        let observations = Arc::clone(&execution);
        let engine = GatewayEngine::<dyn ExecutionStore>::new(execution, providers.clone());
        let transient: Arc<dyn ExecutionStore> = Arc::new(TransientExecutionStore);
        let probe_engine = GatewayEngine::<dyn ExecutionStore>::new(transient, providers.clone());
        Self {
            snapshots,
            coordinator: Arc::new(AttemptCoordinator::new(engine)),
            probe_coordinator: Arc::new(AttemptCoordinator::new(probe_engine)),
            observations,
            providers,
            admissions,
            circuits,
            continuation,
            client_api_key_usage,
        }
    }

    async fn start_inner(
        &self,
        mut request: StartExecution,
    ) -> Result<StartedExecution, GatewayError> {
        request.client.policy.authorize().map_err(|_| {
            GatewayError::new(GatewayErrorKind::PolicyDenied, "client API key is disabled")
        })?;
        let started_at = SystemTime::now();
        let deadline_at = started_at
            .checked_add(MODEL_REQUEST_DEADLINE)
            .ok_or_else(|| {
                GatewayError::new(GatewayErrorKind::Internal, "system clock is invalid")
            })?;
        let request_id = new_request_id()?;
        let routing_context = self
            .route_context(request.client.policy.provider_kind())
            .await?;
        let plan = request
            .client
            .snapshot
            .plan(&request.public_model, &request.operation, &routing_context)
            .map_err(map_routing_error)?;
        let continuation = match request.metadata.previous_response_id.as_ref() {
            Some(previous) => {
                let resolve = self.continuation.resolve(previous).fuse();
                let timeout = Delay::new(COORDINATION_TIMEOUT).fuse();
                pin_mut!(resolve, timeout);
                let pin = select_biased! {
                    result = resolve => result.ok().flatten(),
                    _ = timeout => None,
                };
                match pin {
                    Some(pin) => {
                        attach_continuation_session_state(&mut request.operation, &pin);
                        ContinuationBinding::Pinned(pin)
                    }
                    None => ContinuationBinding::External(previous.clone()),
                }
            }
            None => {
                return self
                    .start_without_continuation(
                        request,
                        request_id,
                        started_at,
                        deadline_at,
                        plan,
                        None,
                    )
                    .await;
            }
        };
        self.start_without_continuation(
            request,
            request_id,
            started_at,
            deadline_at,
            plan,
            Some(continuation),
        )
        .await
    }

    async fn start_without_continuation(
        &self,
        request: StartExecution,
        request_id: ModelRequestId,
        started_at: SystemTime,
        deadline_at: SystemTime,
        plan: crate::routing::RoutingPlan,
        continuation: Option<ContinuationBinding>,
    ) -> Result<StartedExecution, GatewayError> {
        let admission_request = ClientAdmissionRequest {
            model_request_id: request_id.clone(),
            client_api_key_id: request.client.policy.key_id().clone(),
            lease_ttl: MODEL_REQUEST_DEADLINE,
            limits: request.client.policy.limits(),
        };
        match self
            .admissions
            .admit(admission_request)
            .await
            .map_err(|_| {
                GatewayError::new(
                    GatewayErrorKind::NoAvailableProvider,
                    "request admission is temporarily unavailable",
                )
            })? {
            ClientAdmissionDecision::Granted => {}
            ClientAdmissionDecision::Rejected(
                ClientAdmissionRejection::RateLimited
                | ClientAdmissionRejection::ConcurrencyLimited,
            ) => {
                return Err(GatewayError::new(
                    GatewayErrorKind::RateLimited,
                    "request exceeds client API key limits",
                ));
            }
        }
        let admission = AdmissionLease {
            port: Arc::clone(&self.admissions),
            client_api_key_id: request.client.policy.key_id().clone(),
            model_request_id: request_id.clone(),
        };
        let observation = self
            .providers
            .request_observation(request.client.policy.provider_kind(), &request.operation);
        let new_request = NewModelRequest {
            id: request_id.clone(),
            client_api_key_id: Some(request.client.policy.key_id().clone()),
            client_api_key_ref: request.client.policy.key_id().clone(),
            config_revision: plan.config_revision(),
            protocol: request.metadata.protocol,
            operation: request.operation.kind(),
            endpoint: request.metadata.endpoint,
            client_transport: request.metadata.transport.as_str().to_owned(),
            requested_model: request.public_model,
            client_ip: request.metadata.client_ip,
            user_agent: request.metadata.user_agent,
            reasoning_effort: observation.reasoning_effort,
            reasoning_preset: observation.reasoning_preset,
            request_kind: observation.request_kind,
            subagent_kind: observation.subagent_kind,
            compact: observation.compact,
            image_generation_requested: request.operation.image_generation_requested(),
            started_at,
            deadline_at,
        };
        let core = match self
            .coordinator
            .start(
                new_request,
                request.operation,
                plan,
                None,
                continuation,
                CancellationToken::new(),
            )
            .await
        {
            Ok(core) => core,
            Err(error) => {
                admission.release().await;
                return Err(gateway_error_from_engine(&error));
            }
        };
        Ok(StartedExecution {
            request_id,
            created_at: started_at,
            stream: request.metadata.stream,
            session: Box::new(DefaultExecutionSession::new(
                core,
                admission,
                Arc::clone(&self.circuits),
                Arc::clone(&self.continuation),
            )),
        })
    }

    async fn route_context(
        &self,
        provider_kind: &ProviderKind,
    ) -> Result<RoutingContext, GatewayError> {
        let decision = self.circuits.decision(provider_kind).fuse();
        let timeout = Delay::new(COORDINATION_TIMEOUT).fuse();
        pin_mut!(decision, timeout);
        let decision = select_biased! {
            result = decision => Some(result),
            _ = timeout => None,
        };
        let blocked_providers = match decision {
            Some(Ok(ProviderCircuitDecision::Allow)) => BTreeSet::new(),
            Some(Ok(ProviderCircuitDecision::BlockedUntil(_))) => {
                BTreeSet::from([provider_kind.clone()])
            }
            Some(Err(error)) => {
                tracing::warn!(
                    provider = provider_kind.as_str(),
                    %error,
                    "Provider circuit 读取失败，按可重建协调状态 fail-open"
                );
                BTreeSet::new()
            }
            None => {
                tracing::warn!(
                    provider = provider_kind.as_str(),
                    "Provider circuit 读取超时，按可重建协调状态 fail-open"
                );
                BTreeSet::new()
            }
        };
        Ok(RoutingContext {
            provider_kind: Some(provider_kind.clone()),
            blocked_providers,
        })
    }

    async fn probe_inner(
        &self,
        request: AccountProbeRequest,
    ) -> Result<AccountProbeResult, AccountProbeError> {
        let AccountProbeRequest {
            account_id,
            provider_kind,
            upstream_model,
            operation,
        } = request;
        let observed = ProbeObservation {
            provider_kind: provider_kind.clone(),
            account_id: account_id.clone(),
            upstream_model: upstream_model.clone(),
        };
        let snapshot = self.snapshots.acquire().map_err(|_| {
            GatewayError::new(
                GatewayErrorKind::Internal,
                "runtime snapshot is unavailable",
            )
        })?;
        let public_model =
            PublicModelId::new(upstream_model.as_str().to_owned()).map_err(|_| {
                GatewayError::new(GatewayErrorKind::Unsupported, "requested model is invalid")
            })?;
        let routing_context = RoutingContext {
            provider_kind: Some(provider_kind),
            ..RoutingContext::default()
        };
        let plan = snapshot
            .plan(&public_model, &operation, &routing_context)
            .map_err(map_routing_error)?;
        let started_at = SystemTime::now();
        let deadline_at = started_at
            .checked_add(MODEL_REQUEST_DEADLINE)
            .ok_or_else(|| {
                GatewayError::new(GatewayErrorKind::Internal, "system clock is invalid")
            })?;
        let request_id = new_request_id()?;
        let actor = ClientApiKeyId::new("admin_connection_test")
            .map_err(|_| GatewayError::new(GatewayErrorKind::Internal, "invalid admin actor"))?;
        let new_request = NewModelRequest {
            id: request_id,
            client_api_key_id: None,
            client_api_key_ref: actor,
            config_revision: plan.config_revision(),
            protocol: "admin_connection_test".to_owned(),
            operation: operation.kind(),
            endpoint: "/api/admin/accounts/connection-test".to_owned(),
            client_transport: ClientTransport::InternalProbe.as_str().to_owned(),
            requested_model: public_model,
            client_ip: None,
            user_agent: None,
            reasoning_effort: None,
            reasoning_preset: None,
            request_kind: Some("account_connection_test".to_owned()),
            subagent_kind: None,
            compact: false,
            image_generation_requested: false,
            started_at,
            deadline_at,
        };
        let mut session = match self
            .probe_coordinator
            .start_diagnostic(
                new_request,
                operation,
                plan,
                account_id,
                None,
                CancellationToken::new(),
            )
            .await
        {
            Ok(session) => session,
            Err(error) => {
                return Err(self
                    .observe_probe_failure(&observed, started_at, &error)
                    .await);
            }
        };
        let events = session.collect_uncommitted().await;
        publish_provider_attempt_outcomes(
            self.circuits.as_ref(),
            session.provider_attempt_outcomes(),
        )
        .await;
        let events = match events {
            Ok(events) => events,
            Err(error) => {
                return Err(self
                    .observe_probe_failure(&observed, started_at, &error)
                    .await);
            }
        };
        if let Err(error) = session.commit_downstream(Some(200)).await {
            return Err(self
                .observe_probe_failure(&observed, started_at, &error)
                .await);
        }
        Ok(AccountProbeResult {
            text: events
                .into_iter()
                .flat_map(|event| event.into_parts().0)
                .filter_map(|fact| match fact {
                    GatewayEvent::TextDelta(delta) => Some(delta.text),
                    _ => None,
                })
                .collect(),
        })
    }

    /// 探测失败先记录脱敏分类事实，再把请求局部的原始上游响应交给认证管理端。
    async fn observe_probe_failure(
        &self,
        observed: &ProbeObservation,
        started_at: SystemTime,
        error: &EngineError,
    ) -> AccountProbeError {
        let upstream_response = match error {
            EngineError::Provider(provider_error) => provider_error
                .client_visible_upstream_response()
                .map(AccountProbeUpstreamResponse::from_client_response),
            _ => None,
        };
        if let EngineError::Provider(provider_error) = error {
            let latency = started_at.elapsed().unwrap_or_default();
            let latency_ms = u64::try_from(latency.as_millis()).unwrap_or(u64::MAX);
            tracing::warn!(
                target: "gateway_probe",
                provider_kind = observed.provider_kind.as_str(),
                account_id = observed.account_id.as_str(),
                upstream_model = observed.upstream_model.as_str(),
                failure_kind = provider_error.kind().as_str(),
                send_state = ?provider_error.send_state(),
                upstream_status = ?provider_error.upstream_status(),
                provider_error_code = ?provider_error.upstream_code().map(|code| code.as_str()),
                latency_ms,
                "账号连接测试失败"
            );
            if let Err(store_error) = self
                .observations
                .record_probe_failure(ProbeFailure {
                    provider_kind: observed.provider_kind.clone(),
                    account_id: observed.account_id.clone(),
                    upstream_model_id: observed.upstream_model.clone(),
                    error: provider_error.clone(),
                    latency,
                })
                .await
            {
                tracing::warn!(
                    operation = "record_probe_failure",
                    provider_kind = observed.provider_kind.as_str(),
                    account_id = observed.account_id.as_str(),
                    error_kind = ?store_error.kind(),
                    "账号连接测试观测写入失败，测试结果不受影响"
                );
            }
        }
        AccountProbeError::new(gateway_error_from_engine(error), upstream_response)
    }
}

struct ProbeObservation {
    provider_kind: ProviderKind,
    account_id: ProviderAccountId,
    upstream_model: UpstreamModelId,
}

struct TransientExecutionStore;

#[async_trait::async_trait]
impl ExecutionStore for TransientExecutionStore {
    async fn create_model_request(&self, _: NewModelRequest) -> Result<(), StoreError> {
        Ok(())
    }

    async fn record_attempt(&self, _: AttemptRecord) -> Result<(), StoreError> {
        Ok(())
    }

    async fn mark_send_state(
        &self,
        _: &ModelRequestId,
        _: UpstreamSendState,
    ) -> Result<(), StoreError> {
        Ok(())
    }

    async fn mark_downstream_committed(
        &self,
        _: &ModelRequestId,
        _: SystemTime,
        _: Option<u16>,
    ) -> Result<(), StoreError> {
        Ok(())
    }

    async fn record_client_status(&self, _: &ModelRequestId, _: u16) -> Result<(), StoreError> {
        Ok(())
    }

    async fn record_intermediate_failure(&self, _: IntermediateFailure) -> Result<(), StoreError> {
        Ok(())
    }

    async fn finalize_model_request(&self, _: ModelRequestFinalization) -> Result<(), StoreError> {
        Ok(())
    }

    async fn recover_expired(&self, _: SystemTime) -> Result<RecoveryReport, StoreError> {
        Ok(RecoveryReport::default())
    }
}

impl ExecutionService for DefaultExecutionService {
    fn authenticate(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        let snapshot = self
            .snapshots
            .acquire()
            .map_err(|_| ClientAuthenticationError::SnapshotUnavailable)?;
        let policy = snapshot
            .client_policies()
            .filter(|policy| {
                constant_time_equal(plaintext, policy.plaintext_key().expose_for_auth())
            })
            .find(|policy| policy.authorize().is_ok())
            .cloned()
            .ok_or(ClientAuthenticationError::InvalidKey)?;
        self.client_api_key_usage.record_used(policy.key_id());
        Ok(AuthenticatedClient { snapshot, policy })
    }

    fn public_models(&self, client: &AuthenticatedClient) -> Vec<PublicModelId> {
        client
            .snapshot
            .public_models_for_provider(client.policy.provider_kind())
    }

    fn public_model_profiles(&self, client: &AuthenticatedClient) -> Vec<PublicModelProfile> {
        client
            .snapshot
            .public_model_profiles_for_provider(client.policy.provider_kind())
    }

    fn contains_public_model(&self, client: &AuthenticatedClient, model: &PublicModelId) -> bool {
        client
            .snapshot
            .contains_public_model_for_provider(model, client.policy.provider_kind())
    }

    fn start(
        &self,
        request: StartExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async move { self.start_inner(request).await })
    }
}

impl AccountProbe for DefaultExecutionService {
    fn probe(
        &self,
        request: AccountProbeRequest,
    ) -> BoxFuture<'_, Result<AccountProbeResult, AccountProbeError>> {
        Box::pin(async move { self.probe_inner(request).await })
    }
}

struct AdmissionLease {
    port: Arc<dyn ClientAdmissionPort>,
    client_api_key_id: ClientApiKeyId,
    model_request_id: ModelRequestId,
}

impl AdmissionLease {
    async fn release(self) {
        if let Err(error) = self
            .port
            .release(&self.client_api_key_id, &self.model_request_id)
            .await
        {
            tracing::warn!(%error, "Client admission 释放失败，依赖租约 TTL 收敛");
        }
    }
}

struct DefaultExecutionSession {
    core: Option<ResponseExecutionSession<dyn ExecutionStore>>,
    admission: Option<AdmissionLease>,
    circuits: Arc<dyn ProviderCircuitPort>,
    continuation: Arc<dyn NativeContinuationPort>,
    observed_provider_outcomes: usize,
    continuation_recorded: bool,
}

impl DefaultExecutionSession {
    fn new(
        core: ResponseExecutionSession<dyn ExecutionStore>,
        admission: AdmissionLease,
        circuits: Arc<dyn ProviderCircuitPort>,
        continuation: Arc<dyn NativeContinuationPort>,
    ) -> Self {
        Self {
            core: Some(core),
            admission: Some(admission),
            circuits,
            continuation,
            observed_provider_outcomes: 0,
            continuation_recorded: false,
        }
    }

    fn core_mut(
        &mut self,
    ) -> Result<&mut ResponseExecutionSession<dyn ExecutionStore>, EngineError> {
        self.core.as_mut().ok_or(EngineError::InvalidDeliveryState)
    }

    async fn settle_if_finalized(&mut self) {
        if self
            .core
            .as_ref()
            .is_some_and(ResponseExecutionSession::is_finalized)
            && let Some(admission) = self.admission.take()
        {
            admission.release().await;
        }
    }

    async fn observe_provider_outcomes(&mut self) {
        let Some(core) = self.core.as_ref() else {
            return;
        };
        let outcomes = core.provider_attempt_outcomes();
        let new_outcomes = outcomes
            .get(self.observed_provider_outcomes..)
            .unwrap_or_default()
            .to_vec();
        self.observed_provider_outcomes = outcomes.len();
        publish_provider_attempt_outcomes(self.circuits.as_ref(), &new_outcomes).await;
    }

    async fn record_continuation(&mut self, state: Option<&ProviderSessionState>) {
        if self.continuation_recorded {
            return;
        }
        let Some(state) = state else {
            return;
        };
        let Some(pin) = self
            .core
            .as_ref()
            .and_then(|core| core.native_continuation_pin(state))
        else {
            return;
        };
        self.continuation_recorded = true;
        record_native_continuation(self.continuation.as_ref(), pin).await;
    }

    async fn finalize_detached(&mut self) {
        let Some(mut core) = self.core.take() else {
            return;
        };
        core.cancel();
        if !core.is_finalized()
            && let Err(error) = core.cancel_and_finalize().await
        {
            tracing::warn!(%error, "Detached execution 终态收敛失败");
        }
        let pending = core
            .provider_attempt_outcomes()
            .get(self.observed_provider_outcomes..)
            .unwrap_or_default();
        publish_provider_attempt_outcomes(self.circuits.as_ref(), pending).await;
        if let Some(admission) = self.admission.take() {
            admission.release().await;
        }
    }
}

impl Drop for DefaultExecutionSession {
    fn drop(&mut self) {
        if let Some(core) = &self.core {
            core.cancel();
        }
    }
}

impl ExecutionSession for DefaultExecutionSession {
    fn next_event(&mut self) -> BoxFuture<'_, Result<Option<CoordinatedEvent>, EngineError>> {
        Box::pin(async move {
            let result = self.core_mut()?.next_event().await;
            if let Ok(Some(event)) = result.as_ref() {
                self.record_continuation(event.session_update()).await;
            }
            self.observe_provider_outcomes().await;
            self.settle_if_finalized().await;
            result
        })
    }

    fn collect_uncommitted(&mut self) -> BoxFuture<'_, Result<Vec<ProviderEvent>, EngineError>> {
        Box::pin(async move {
            let result = self.core_mut()?.collect_uncommitted().await;
            if let Ok(events) = result.as_ref() {
                let state = events.iter().find_map(ProviderEvent::session_update);
                self.record_continuation(state).await;
            }
            self.observe_provider_outcomes().await;
            self.settle_if_finalized().await;
            result
        })
    }

    fn response_headers(&self) -> &[ProviderResponseHeader] {
        self.core
            .as_ref()
            .map(ResponseExecutionSession::response_headers)
            .unwrap_or_default()
    }

    fn commit_downstream(
        &mut self,
        client_status_code: Option<u16>,
    ) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            let result = self.core_mut()?.commit_downstream(client_status_code).await;
            self.observe_provider_outcomes().await;
            self.settle_if_finalized().await;
            result
        })
    }

    fn record_client_status(
        &mut self,
        client_status_code: u16,
    ) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            let result = self
                .core_mut()?
                .record_client_status(client_status_code)
                .await;
            self.observe_provider_outcomes().await;
            self.settle_if_finalized().await;
            result
        })
    }

    fn is_finalized(&self) -> bool {
        self.core
            .as_ref()
            .is_none_or(ResponseExecutionSession::is_finalized)
    }

    fn cancel(&self) {
        if let Some(core) = &self.core {
            core.cancel();
        }
    }

    fn detach_finalize(mut self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move { self.finalize_detached().await })
    }
}

#[must_use]
pub const fn provider_failure_affects_circuit(error_kind: ProviderErrorKind) -> bool {
    matches!(
        error_kind,
        ProviderErrorKind::Timeout
            | ProviderErrorKind::Transport
            | ProviderErrorKind::Protocol
            | ProviderErrorKind::Unavailable
    )
}

async fn publish_provider_attempt_outcomes(
    circuits: &dyn ProviderCircuitPort,
    outcomes: &[ProviderAttemptOutcome],
) {
    for outcome in outcomes {
        let result = match outcome.error_kind() {
            None => circuits.observe_success(outcome.provider_kind()).await,
            Some(kind) if provider_failure_affects_circuit(kind) => {
                circuits.observe_failure(outcome.provider_kind()).await
            }
            Some(_) => continue,
        };
        if let Err(error) = result {
            tracing::warn!(
                provider = outcome.provider_kind().as_str(),
                %error,
                "Provider circuit feedback 写入失败，数据面不受影响"
            );
        }
    }
}

async fn record_native_continuation(
    continuation: &dyn NativeContinuationPort,
    pin: NativeContinuationPin,
) {
    let record = continuation.record(pin).fuse();
    let timeout = Delay::new(COORDINATION_TIMEOUT).fuse();
    pin_mut!(record, timeout);
    select_biased! {
        result = record => {
            if let Err(error) = result {
                tracing::warn!(%error, "Continuation affinity 写入失败，后续请求将退化为外部续接");
            }
        },
        _ = timeout => {
            tracing::warn!("Continuation affinity 后台写入超时，已丢弃本次亲和记录");
        },
    }
}

fn constant_time_equal(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.as_bytes()
        .iter()
        .zip(right.as_bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn new_request_id() -> Result<ModelRequestId, GatewayError> {
    ModelRequestId::new(format!("req_{}", Uuid::now_v7().simple()))
        .map_err(|_| GatewayError::new(GatewayErrorKind::Internal, "failed to allocate request ID"))
}

fn map_routing_error(error: crate::error::RoutingError) -> GatewayError {
    match error {
        crate::error::RoutingError::NoCapableProvider { .. } => GatewayError::new(
            GatewayErrorKind::NoAvailableProvider,
            "no provider can execute this request",
        ),
        _ => GatewayError::new(
            GatewayErrorKind::Internal,
            "runtime routing configuration is invalid",
        ),
    }
}

fn attach_continuation_session_state(operation: &mut Operation, pin: &NativeContinuationPin) {
    let Some(state) = pin.session_state() else {
        return;
    };
    if state.provider() != pin.provider().as_str()
        || operation.provider_session_state(state.provider()).is_some()
    {
        return;
    }
    operation.set_provider_session_state(state.clone());
}

pub fn gateway_error_from_engine(error: &EngineError) -> GatewayError {
    match error {
        EngineError::Cancelled => {
            GatewayError::new(GatewayErrorKind::Cancelled, "request was cancelled")
        }
        EngineError::Deadline => {
            GatewayError::new(GatewayErrorKind::Timeout, "request deadline elapsed")
        }
        EngineError::Provider(provider) => GatewayError::from_provider(provider),
        EngineError::EmptyRoutingPlan | EngineError::ProviderNotRegistered { .. } => {
            GatewayError::new(
                GatewayErrorKind::NoAvailableProvider,
                "no provider is available",
            )
        }
        _ => GatewayError::new(GatewayErrorKind::Internal, "request execution failed"),
    }
}
