//! 数据面执行用例：认证、准入、路由、continuation、circuit 与会话生命周期。

use std::collections::BTreeSet;
use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::future::BoxFuture;
use uuid::Uuid;

use crate::engine::admission::{
    ClientAdmissionDecision, ClientAdmissionPort, ClientAdmissionRejection, ClientAdmissionRequest,
};
use crate::engine::continuation::{
    ContinuationBinding, NativeContinuationPort, PreviousResponseId,
};
use crate::engine::coordinator::ResponseExecutionSession;
use crate::engine::probe::{AccountProbe, AccountProbeRequest, AccountProbeResult};
use crate::engine::provider::ProviderRegistry;
use crate::engine::{
    AttemptCoordinator, AttemptRecord, CancellationToken, CommitRequirement, CoordinatedEvent,
    EngineError, ExecutionStore, GatewayEngine, IntermediateFailure, ModelRequestFinalization,
    ModelRequestId, NewModelRequest, ProviderAttemptOutcome, RecoveryReport, UpstreamSendState,
};
use crate::error::{GatewayError, GatewayErrorKind, ProviderErrorKind, StoreError};
use crate::event::{GatewayEvent, ProviderEvent, ProviderResponseHeader};
use crate::operation::{Operation, ReasoningEffort};
use crate::policy::{ClientApiKeyId, ClientPolicy};
use crate::routing::snapshot::RuntimeSnapshotHandle;
use crate::routing::{ProviderKind, PublicModelId, RoutingContext, RuntimeSnapshot};

const MODEL_REQUEST_DEADLINE: Duration = Duration::from_secs(10 * 60);

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
    fn contains_public_model(&self, client: &AuthenticatedClient, model: &PublicModelId) -> bool;
    fn start(
        &self,
        request: StartExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCircuitDecision {
    Allow,
    BlockedUntil(SystemTime),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("provider circuit store is unavailable")]
pub struct ProviderCircuitError;

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
    providers: ProviderRegistry,
    admissions: Arc<dyn ClientAdmissionPort>,
    circuits: Arc<dyn ProviderCircuitPort>,
    continuation: Arc<dyn NativeContinuationPort>,
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
    ) -> Self {
        let engine = GatewayEngine::<dyn ExecutionStore>::new(execution, providers.clone());
        let transient: Arc<dyn ExecutionStore> = Arc::new(TransientExecutionStore);
        let probe_engine = GatewayEngine::<dyn ExecutionStore>::new(transient, providers.clone());
        Self {
            snapshots,
            coordinator: Arc::new(AttemptCoordinator::new(engine)),
            probe_coordinator: Arc::new(AttemptCoordinator::new(probe_engine)),
            providers,
            admissions,
            circuits,
            continuation,
        }
    }

    async fn start_inner(&self, request: StartExecution) -> Result<StartedExecution, GatewayError> {
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
            Some(previous) => match self
                .continuation
                .resolve(request.client.policy.key_id(), previous)
                .await
                .map_err(|_| {
                    GatewayError::new(
                        GatewayErrorKind::Internal,
                        "previous response history is temporarily unavailable",
                    )
                })? {
                Some(pin) => ContinuationBinding::Pinned(pin),
                None => ContinuationBinding::External(previous.clone()),
            },
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
        let input_token_estimate = request
            .operation
            .capability_requirements()
            .minimum_context_tokens();
        let admission_request = ClientAdmissionRequest {
            model_request_id: request_id.clone(),
            client_api_key_id: request.client.policy.key_id().clone(),
            input_token_estimate,
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
        let reasoning_effort = match &request.operation {
            Operation::Generate(generate) => generate
                .reasoning()
                .and_then(|reasoning| reasoning.effort)
                .map(reasoning_effort_name)
                .map(str::to_owned),
            Operation::CompactConversation(compact) => compact
                .generation()
                .reasoning()
                .and_then(|reasoning| reasoning.effort)
                .map(reasoning_effort_name)
                .map(str::to_owned),
            _ => None,
        };
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
            input_token_estimate,
            client_ip: request.metadata.client_ip,
            user_agent: request.metadata.user_agent,
            reasoning_effort,
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
            )),
        })
    }

    async fn route_context(
        &self,
        provider_kind: &ProviderKind,
    ) -> Result<RoutingContext, GatewayError> {
        let blocked_providers = match self.circuits.decision(provider_kind).await.map_err(|_| {
            GatewayError::new(
                GatewayErrorKind::NoAvailableProvider,
                "provider health state is temporarily unavailable",
            )
        })? {
            ProviderCircuitDecision::Allow => BTreeSet::new(),
            ProviderCircuitDecision::BlockedUntil(_) => BTreeSet::from([provider_kind.clone()]),
        };
        Ok(RoutingContext {
            provider_kind: Some(provider_kind.clone()),
            blocked_providers,
        })
    }

    async fn probe_inner(
        &self,
        request: AccountProbeRequest,
    ) -> Result<AccountProbeResult, GatewayError> {
        let AccountProbeRequest {
            account_id,
            provider_kind,
            upstream_model,
            operation,
        } = request;
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
        let input_token_estimate = operation.capability_requirements().minimum_context_tokens();
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
            input_token_estimate,
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
        let mut session = self
            .probe_coordinator
            .start(
                new_request,
                operation,
                plan,
                Some(account_id),
                None,
                CancellationToken::new(),
            )
            .await
            .map_err(|error| gateway_error_from_engine(&error))?;
        let events = session.collect_uncommitted().await;
        publish_provider_attempt_outcomes(
            self.circuits.as_ref(),
            session.provider_attempt_outcomes(),
        )
        .await;
        let events = events.map_err(|error| gateway_error_from_engine(&error))?;
        session
            .commit_downstream(Some(200))
            .await
            .map_err(|error| gateway_error_from_engine(&error))?;
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
        Ok(AuthenticatedClient { snapshot, policy })
    }

    fn public_models(&self, client: &AuthenticatedClient) -> Vec<PublicModelId> {
        client
            .snapshot
            .public_models_for_provider(client.policy.provider_kind())
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
    ) -> BoxFuture<'_, Result<AccountProbeResult, GatewayError>> {
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
        let _ = self
            .port
            .release(&self.client_api_key_id, &self.model_request_id)
            .await;
    }
}

struct DefaultExecutionSession {
    core: Option<ResponseExecutionSession<dyn ExecutionStore>>,
    admission: Option<AdmissionLease>,
    circuits: Arc<dyn ProviderCircuitPort>,
    observed_provider_outcomes: usize,
}

impl DefaultExecutionSession {
    fn new(
        core: ResponseExecutionSession<dyn ExecutionStore>,
        admission: AdmissionLease,
        circuits: Arc<dyn ProviderCircuitPort>,
    ) -> Self {
        Self {
            core: Some(core),
            admission: Some(admission),
            circuits,
            observed_provider_outcomes: 0,
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

    async fn finalize_detached(&mut self) {
        let Some(mut core) = self.core.take() else {
            return;
        };
        core.cancel();
        if !core.is_finalized() {
            let _ = core.cancel_and_finalize().await;
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
            self.observe_provider_outcomes().await;
            self.settle_if_finalized().await;
            result
        })
    }

    fn collect_uncommitted(&mut self) -> BoxFuture<'_, Result<Vec<ProviderEvent>, EngineError>> {
        Box::pin(async move {
            let result = self.core_mut()?.collect_uncommitted().await;
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
        let _ = result;
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

const fn reasoning_effort_name(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::ExtraHigh => "xhigh",
    }
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

#[must_use]
pub const fn commit_requirement(event: &CoordinatedEvent) -> CommitRequirement {
    event.commit_requirement()
}
