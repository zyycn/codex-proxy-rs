//! Codex 的 `gateway-core` Provider adapter。

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{StreamExt, future::BoxFuture};
use gateway_core::engine::continuation::{ContinuationBinding, NativeContinuationScope};
use gateway_core::engine::credential::{AccountFeedbackStats, ProviderAccount};
use gateway_core::engine::provider::{
    EventStream, Provider, ProviderCallMetadata, ProviderCatalogGeneration,
    ProviderModelCapabilities, ProviderRequest, ProviderRequestObservation, ProviderStream,
    UpstreamTransport,
};
use gateway_core::engine::{
    AttemptContext, CancellationToken, ContinuationAttempt, UpstreamSendState,
};
use gateway_core::error::{
    ClientVisibleUpstreamError, ClientVisibleUpstreamResponse, ContinuationFailure,
    OpaqueUpstreamValue, ProviderError, ProviderErrorKind,
};
use gateway_core::event::{
    FinishReason, GatewayEvent, ProtocolWireEvent, ProviderEvent, ProviderResponseHeader,
    ProviderResponseMetadata, ProviderResponseObservation, ProviderResponseTimings, ResponseMeta,
    UpstreamHttpVersion, WebSocketPoolKind,
};
use gateway_core::operation::{
    GenerateRequest, ImageRequest, ImageRequestKind, Operation, OperationKind, ProviderSessionState,
};
use gateway_core::provider_ports::ProviderSessionAffinityKey;
use gateway_core::routing::{
    ModelCapabilities, ModelPresentation, ProviderCandidate, ProviderKind, UpstreamModelId,
};
use gateway_core::task::{
    DaemonRestartPolicy, DaemonTask, ScheduledTask, WorkerContribution, WorkerCycleContext,
    WorkerDefinitionError, WorkerId, WorkerKind, WorkerLeaseRequest, WorkerRegistration,
    WorkerRunnable, WorkerSchedule, WorkerTaskError,
};
use gateway_protocol::openai::events::{
    ParsedRateLimits, parse_rate_limit_headers, rate_limits_to_header_pairs,
};
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use url::Url;

use crate::credential::{
    CodexAccountFailure, CodexCredentialCatalogError, CodexCredentialCatalogService,
    CodexCredentialLease, CodexCredentialQuotaService, CodexCredentialRefreshOutcome,
    CodexCredentialRefreshService, CodexCredentialSelector, CodexCyberPolicyScope,
    CodexQuotaRefreshPolicy, CodexSessionAffinity, CredentialSelectionError, RuntimeCodexCookie,
    SelectCodexCredential, SelectCodexProviderEndpointCredential,
    derive_codex_cyber_policy_session_key, derive_codex_session_affinity,
};
use crate::transport::canonical::{
    CodexCanonicalDecoder, CodexCanonicalError, CodexCanonicalOutcome,
};
use crate::transport::catalog::{
    CodexCatalogCapabilityEvidence, CodexCatalogModel, CodexCatalogVisibility,
};
use crate::transport::diagnostics::{
    CodexFailureCategory, CodexUpstreamFailure, CodexUpstreamSendPhase,
};
use crate::transport::profile::{
    APPCAST_POLL_INTERVAL, CodexDesktopReleaseService, CodexWireProfileState,
};
use crate::transport::protocol::responses::{
    CodexResponsesRequest, PreviousResponseScope, ResponseEventSignals,
};
use crate::transport::protocol::websocket::{
    PREVIOUS_RESPONSE_NOT_FOUND_CODE, PREVIOUS_RESPONSE_NOT_FOUND_MESSAGE,
};
use crate::transport::request::{
    CodexRequestEncodeError, RequestAccountScope, encode_generate_request, scope_request_to_account,
};
use crate::transport::session::CodexSessionIdentity;
use crate::transport::usage::normalize_service_tier;
use crate::transport::websocket::{CodexWebSocketExchangeError, PreviousResponseUnavailableReason};
use crate::transport::{
    CODEX_IMAGE_EDITS_PATH, CODEX_IMAGE_GENERATIONS_PATH, CODEX_RESPONSES_PATH,
    CodexAccountSelectionTelemetry, CodexBackendClient, CodexBackendJsonResponse,
    CodexBackendStreamingResponse, CodexBackendTransport, CodexClientError, CodexRateLimitUpdates,
    CodexRequestContext, CodexResponseMetadata, CodexTransportMetrics, CodexUpstreamDiagnostics,
    CodexWebSocketPool, endpoint_url,
};

const PROVIDER_NAME: &str = "openai";
const HTTP_SSE_TRANSPORT: &str = "http_sse";
const HTTP_JSON_TRANSPORT: &str = "http_json";
const WEBSOCKET_TRANSPORT: &str = "websocket";
const MAX_COOKIE_HEADER_BYTES: usize = 16 * 1024;
/// 提交边界前最多保留 64 KiB 原始上游 chunk；达到阈值后结束无感换号窗口，
/// 但不会把上游数据改写成协议失败。
const MAX_STREAM_PREFETCH_BYTES: usize = 64 * 1024;
/// 短暂保留 response.created 等结构事件，让随后到达的明确拒绝可以无感换号；
/// 到期即放行，避免模型长时间思考时让客户端一直收不到首事件。
const STREAM_REPLAY_GRACE: Duration = Duration::from_millis(1_200);
// 额度拒绝后先给上游额度结算留出时间，再以受限时长同步 usage 快照。
const QUOTA_FAILURE_REFRESH_DELAY: Duration = Duration::from_secs(2);
const QUOTA_FAILURE_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
pub const OFFICIAL_CODEX_BASE_PATH: &str = "/backend-api";
pub const OFFICIAL_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexProviderTransport {
    HttpOnly,
    PreferWebSocket,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodexProviderConfigError {
    #[error("Codex provider URL is invalid")]
    InvalidBaseUrl,
}

pub struct CodexProvider {
    selector: Arc<CodexCredentialSelector>,
    catalog: Arc<CodexCredentialCatalogService>,
    quota: Arc<CodexCredentialQuotaService>,
    account_feedback: Arc<AccountFeedbackStats>,
    client: CodexBackendClient,
    responses_url: Url,
    image_generations_url: Url,
    image_edits_url: Url,
    session_identity: Option<CodexSessionIdentity>,
}

impl CodexProvider {
    // Provider 构造集中装配独立领域服务和透明传输依赖，拆分参数会模糊所有权。
    #[expect(clippy::too_many_arguments)]
    pub fn new(
        selector: Arc<CodexCredentialSelector>,
        catalog: Arc<CodexCredentialCatalogService>,
        quota: Arc<CodexCredentialQuotaService>,
        account_feedback: Arc<AccountFeedbackStats>,
        http: Client,
        profile: CodexWireProfileState,
        base_url: String,
        websocket_pool: Arc<CodexWebSocketPool>,
    ) -> Result<Self, CodexProviderConfigError> {
        let responses_url = Url::parse(&endpoint_url(&base_url, CODEX_RESPONSES_PATH))
            .map_err(|_| CodexProviderConfigError::InvalidBaseUrl)?;
        let image_generations_url =
            Url::parse(&endpoint_url(&base_url, CODEX_IMAGE_GENERATIONS_PATH))
                .map_err(|_| CodexProviderConfigError::InvalidBaseUrl)?;
        let image_edits_url = Url::parse(&endpoint_url(&base_url, CODEX_IMAGE_EDITS_PATH))
            .map_err(|_| CodexProviderConfigError::InvalidBaseUrl)?;
        let client =
            CodexBackendClient::new(http, base_url, profile).with_websocket_pool(websocket_pool);
        Ok(Self {
            selector,
            catalog,
            quota,
            account_feedback,
            client,
            responses_url,
            image_generations_url,
            image_edits_url,
            session_identity: None,
        })
    }

    pub(crate) fn with_session_identity(mut self, identity: CodexSessionIdentity) -> Self {
        self.session_identity = Some(identity);
        self
    }
}

impl fmt::Debug for CodexProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexProvider")
            .field("selector", &"<account-selector>")
            .field("catalog", &"<ttl-catalog>")
            .finish()
    }
}

#[async_trait]
impl Provider for CodexProvider {
    fn name(&self) -> &'static str {
        PROVIDER_NAME
    }

    fn catalog_generation(&self) -> ProviderCatalogGeneration {
        self.catalog.catalog_generation()
    }

    fn request_observation(&self, operation: &Operation) -> ProviderRequestObservation {
        let Operation::Generate(request) = operation else {
            return ProviderRequestObservation::default();
        };
        let (semantics, reasoning_effort) = encode_generate_request(request, "observability")
            .map(|encoded| {
                let semantics = encoded.semantics();
                let reasoning_effort = semantics.reasoning_effort.clone();
                (semantics, reasoning_effort)
            })
            .unwrap_or_default();
        ProviderRequestObservation {
            reasoning_effort,
            reasoning_preset: semantics.reasoning_preset.map(str::to_owned),
            request_kind: semantics.request_kind,
            subagent_kind: semantics.subagent_kind,
            compact: semantics.compact,
        }
    }

    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        let snapshot = self.catalog.synchronize().await.map_err(|_| {
            provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent)
        })?;
        Ok(snapshot
            .models()
            .iter()
            .map(compile_model_capabilities)
            .collect())
    }

    async fn execute(
        &self,
        request: ProviderRequest,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        if request.candidate().provider().as_str() != PROVIDER_NAME {
            return Err(provider_error(
                ProviderErrorKind::InvalidRequest,
                UpstreamSendState::NotSent,
            ));
        }
        let candidate = request.candidate();
        if context.cancellation().is_cancelled() {
            return Err(provider_error(
                ProviderErrorKind::Cancelled,
                UpstreamSendState::NotSent,
            ));
        }
        if remaining(context.deadline()).is_none() {
            return Err(provider_error(
                ProviderErrorKind::Timeout,
                UpstreamSendState::NotSent,
            ));
        }
        if let Operation::GenerateImage(image) = request.operation() {
            return self.execute_image(image, candidate, context).await;
        }
        let Operation::Generate(generate) = request.operation() else {
            return Err(provider_error(
                ProviderErrorKind::Unsupported,
                UpstreamSendState::NotSent,
            ));
        };
        let Some(upstream_model) = candidate.upstream_model() else {
            return Err(provider_error(
                ProviderErrorKind::Protocol,
                UpstreamSendState::NotSent,
            ));
        };
        let previous_session = decode_openai_session_state(generate);
        let continuation_requested = generate.native_continuation_requested();
        let mut upstream_request = encode_generate_request(generate, upstream_model.as_str())
            .map_err(map_request_error)?;
        if let Some(conversation_id) = previous_session
            .as_ref()
            .and_then(|state| state.conversation_id.as_ref())
        {
            upstream_request.local_conversation_id = Some(conversation_id.clone());
        }
        if let Some(identity) = &self.session_identity {
            identity.prepare_local_conversation(&mut upstream_request);
        }
        if previous_session.as_ref().is_some_and(|state| {
            same_client_turn(
                state.client_turn_id.as_deref(),
                upstream_request.client_turn_id.as_deref(),
            )
        }) {
            upstream_request.turn_state = previous_session
                .as_ref()
                .and_then(|state| state.turn_state.clone());
        }
        let session_affinity =
            derive_codex_session_affinity(&upstream_request, context.client_api_key_ref());
        let cyber_policy_session_key =
            derive_codex_cyber_policy_session_key(&upstream_request, context.client_api_key_ref());
        let transport = selected_transport(&upstream_request);
        apply_transport(&mut upstream_request, transport);

        let lease = self
            .selector
            .select_with_cyber_policy(
                &SelectCodexCredential {
                    upstream_model: upstream_model.as_str(),
                    request_url: &self.responses_url,
                    attempt: &context,
                    session_affinity_key: session_affinity.as_ref().map(|affinity| affinity.key()),
                },
                cyber_policy_session_key.as_ref(),
                session_affinity.as_ref(),
            )
            .await
            .map_err(map_selection_error)?;
        let lease = Arc::new(lease);
        // 首字计时的起点：账号选择完成之后、上游建立之前。
        let output_started_at = Instant::now();
        let provider_kind = ProviderKind::new(PROVIDER_NAME)
            .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent))?;
        let state_owner_cross_account = context
            .account_state_owner()
            .is_some_and(|owner| !owner.matches(&provider_kind, lease.account_id()))
            || previous_session
                .as_ref()
                .is_some_and(|state| state.account_id != lease.account_id().as_str());
        let has_explicit_state_owner =
            context.account_state_owner().is_some() || previous_session.is_some();
        let account_scope =
            if state_owner_cross_account || (!has_explicit_state_owner && lease.account_switch()) {
                RequestAccountScope::Different
            } else if has_explicit_state_owner || lease.affinity_hit() {
                RequestAccountScope::Same
            } else {
                RequestAccountScope::Unknown
            };
        if context.continuation_attempt() != ContinuationAttempt::None
            && let Some(continuation) = context.continuation()
        {
            match continuation {
                ContinuationBinding::Pinned(continuation) => {
                    let previous_response_scope = match context.continuation_attempt() {
                        ContinuationAttempt::Native => match previous_session
                            .as_ref()
                            .map(|state| state.continuation_scope)
                        {
                            Some(OpenAiContinuationScope::Persisted) => {
                                PreviousResponseScope::Persisted
                            }
                            Some(OpenAiContinuationScope::ConnectionLocal) => {
                                PreviousResponseScope::ConnectionLocal
                            }
                            Some(OpenAiContinuationScope::ReplayRequired) => {
                                PreviousResponseScope::ExternalUnknown
                            }
                            None => match continuation.scope() {
                                NativeContinuationScope::Persisted => {
                                    PreviousResponseScope::Persisted
                                }
                                NativeContinuationScope::ConnectionLocal => {
                                    PreviousResponseScope::ConnectionLocal
                                }
                            },
                        },
                        ContinuationAttempt::ReplayOwner | ContinuationAttempt::ReplayAny => {
                            PreviousResponseScope::ExternalUnknown
                        }
                        ContinuationAttempt::None => PreviousResponseScope::ExternalUnknown,
                    };
                    upstream_request.set_previous_response_id(Some(
                        continuation.upstream_response_id().as_str().to_owned(),
                    ));
                    upstream_request.previous_response_scope = Some(previous_response_scope);
                }
                ContinuationBinding::External(previous_response_id) => {
                    upstream_request
                        .set_previous_response_id(Some(previous_response_id.as_str().to_owned()));
                    upstream_request.previous_response_scope =
                        Some(PreviousResponseScope::ExternalUnknown);
                }
            }
        }
        if upstream_request.previous_response_id().is_some()
            && !account_scope.can_reuse_account_state()
        {
            return Err(continuation_replay_required_error());
        }
        scope_request_to_account(
            &mut upstream_request,
            lease.installation_id(),
            account_scope,
        );
        let metadata = ProviderCallMetadata::new(
            provider_kind,
            upstream_model.clone(),
            lease.account_id().clone(),
            UpstreamTransport::new(transport_name(transport)).map_err(|_| {
                provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
            })?,
        );
        let response_store = upstream_request.store();
        let session_capture =
            (!continuation_requested || previous_session.is_some()).then(|| OpenAiSessionCapture {
                account_id: lease.account_id().as_str().to_owned(),
                conversation_id: upstream_request.local_conversation_id.clone(),
                turn_state: upstream_request.turn_state.clone(),
                client_turn_id: upstream_request.client_turn_id.clone(),
                response_store,
                continuation_scope: None,
            });
        let allows_account_state_mutation = lease.allows_account_state_mutation();
        let session_affinity_key = session_affinity.map(CodexSessionAffinity::into_key);
        let events = cold_response_stream(ColdResponse {
            client: self.client.clone(),
            response_origin: self.responses_url.clone(),
            request: upstream_request,
            upstream_model: upstream_model.clone(),
            transport_policy: transport,
            context,
            selector: Arc::clone(&self.selector),
            quota: Arc::clone(&self.quota),
            catalog: Arc::clone(&self.catalog),
            lease: Arc::clone(&lease),
            output_started_at,
            session_affinity_key,
            session_capture,
        });
        let stream = ProviderStream::new(metadata, events, lease);
        Ok(if allows_account_state_mutation {
            stream.with_account_feedback(Arc::clone(&self.account_feedback))
        } else {
            stream
        })
    }
}

impl CodexProvider {
    async fn execute_image(
        &self,
        image: &ImageRequest,
        candidate: &ProviderCandidate,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        if image.payload().protocol() != PROVIDER_NAME || candidate.upstream_model().is_some() {
            return Err(provider_error(
                ProviderErrorKind::InvalidRequest,
                UpstreamSendState::NotSent,
            ));
        }
        let (endpoint_path, response_origin) = match image.kind() {
            ImageRequestKind::Generation => (
                CODEX_IMAGE_GENERATIONS_PATH,
                self.image_generations_url.clone(),
            ),
            ImageRequestKind::Edit => (CODEX_IMAGE_EDITS_PATH, self.image_edits_url.clone()),
        };
        let lease = self
            .selector
            .select_for_provider_endpoint(&SelectCodexProviderEndpointCredential {
                request_url: &response_origin,
                attempt: &context,
            })
            .await
            .map_err(map_selection_error)?;
        let lease = Arc::new(lease);
        let allows_account_state_mutation = lease.allows_account_state_mutation();
        let provider_kind = ProviderKind::new(PROVIDER_NAME)
            .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent))?;
        let metadata = ProviderCallMetadata::for_provider_endpoint(
            provider_kind,
            lease.account_id().clone(),
            UpstreamTransport::new(HTTP_JSON_TRANSPORT).map_err(|_| {
                provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
            })?,
        );
        let image_turn_id = image
            .payload()
            .context()
            .get("image_turn_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let events = cold_json_response_stream(ColdJsonResponse {
            client: self.client.clone(),
            response_origin,
            endpoint_path,
            body: image.payload().body().clone(),
            image_turn_id,
            context,
            selector: Arc::clone(&self.selector),
            quota: Arc::clone(&self.quota),
            lease: Arc::clone(&lease),
            output_started_at: Instant::now(),
        });
        let stream = ProviderStream::new(metadata, events, lease);
        Ok(if allows_account_state_mutation {
            stream.with_account_feedback(Arc::clone(&self.account_feedback))
        } else {
            stream
        })
    }
}

struct ColdResponse {
    client: CodexBackendClient,
    response_origin: Url,
    request: CodexResponsesRequest,
    upstream_model: UpstreamModelId,
    transport_policy: CodexProviderTransport,
    context: AttemptContext,
    selector: Arc<CodexCredentialSelector>,
    quota: Arc<CodexCredentialQuotaService>,
    catalog: Arc<CodexCredentialCatalogService>,
    lease: Arc<CodexCredentialLease>,
    output_started_at: Instant,
    session_affinity_key: Option<ProviderSessionAffinityKey>,
    session_capture: Option<OpenAiSessionCapture>,
}

struct ColdJsonResponse {
    client: CodexBackendClient,
    response_origin: Url,
    endpoint_path: &'static str,
    body: Bytes,
    image_turn_id: Option<String>,
    context: AttemptContext,
    selector: Arc<CodexCredentialSelector>,
    quota: Arc<CodexCredentialQuotaService>,
    lease: Arc<CodexCredentialLease>,
    output_started_at: Instant,
}

#[derive(Clone, Serialize, Deserialize)]
struct OpenAiSessionState {
    account_id: String,
    conversation_id: Option<String>,
    #[serde(default)]
    turn_state: Option<String>,
    #[serde(default)]
    client_turn_id: Option<String>,
    continuation_scope: OpenAiContinuationScope,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OpenAiContinuationScope {
    Persisted,
    ConnectionLocal,
    ReplayRequired,
}

struct OpenAiSessionCapture {
    account_id: String,
    conversation_id: Option<String>,
    turn_state: Option<String>,
    client_turn_id: Option<String>,
    response_store: bool,
    continuation_scope: Option<OpenAiContinuationScope>,
}

fn same_client_turn(previous: Option<&str>, current: Option<&str>) -> bool {
    previous
        .zip(current)
        .is_some_and(|(previous, current)| !previous.is_empty() && previous == current)
}

fn decode_openai_session_state(request: &GenerateRequest) -> Option<OpenAiSessionState> {
    request
        .provider_session_state(PROVIDER_NAME)
        .and_then(|state| serde_json::from_value(Value::Object(state.payload().clone())).ok())
}

fn encode_openai_session_state(
    state: OpenAiSessionState,
) -> Result<ProviderSessionState, ProviderError> {
    let Value::Object(payload) = serde_json::to_value(state)
        .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent))?
    else {
        return Err(provider_error(
            ProviderErrorKind::Protocol,
            UpstreamSendState::Sent,
        ));
    };
    ProviderSessionState::new(PROVIDER_NAME, payload)
        .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent))
}

fn attach_openai_session_update(
    events: &mut [ProviderEvent],
    capture: &mut Option<OpenAiSessionCapture>,
) {
    let Some(terminal_index) = events
        .iter()
        .position(|event| terminal_response_output(event).is_some())
    else {
        return;
    };
    let Some(capture) = capture.take() else {
        return;
    };
    let Some(continuation_scope) = capture.continuation_scope else {
        return;
    };
    let Ok(update) = encode_openai_session_state(OpenAiSessionState {
        account_id: capture.account_id,
        conversation_id: capture.conversation_id,
        turn_state: capture.turn_state,
        client_turn_id: capture.client_turn_id,
        continuation_scope,
    }) else {
        return;
    };
    events[terminal_index].attach_session_update(update);
}

fn terminal_response_output(event: &ProviderEvent) -> Option<&[Value]> {
    let wire = event.wire_event()?;
    if wire.protocol() != PROVIDER_NAME {
        return None;
    }
    let event_type = wire
        .event_type()
        .or_else(|| wire.data().get("type").and_then(Value::as_str));
    matches!(
        event_type,
        Some("response.completed" | "response.incomplete")
    )
    .then(|| {
        wire.data()
            .pointer("/response/output")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
    })
    .flatten()
}

enum CodexHandshakeAttemptError {
    Client(CodexClientError),
    Cancelled,
    Timeout,
}

async fn create_response_attempt(
    client: &CodexBackendClient,
    request: &CodexResponsesRequest,
    request_context: CodexRequestContext<'_>,
    account_id: &str,
    deadline: SystemTime,
    cancellation: &CancellationToken,
) -> Result<CodexBackendStreamingResponse, CodexHandshakeAttemptError> {
    let Some(handshake_deadline) = remaining(deadline) else {
        return Err(CodexHandshakeAttemptError::Timeout);
    };
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(CodexHandshakeAttemptError::Cancelled),
        _ = tokio::time::sleep(handshake_deadline) => Err(CodexHandshakeAttemptError::Timeout),
        response = client.create_response_stream_with_pool_account(
            request,
            request_context,
            Some(account_id),
        ) => response.map_err(CodexHandshakeAttemptError::Client),
    }
}

fn map_handshake_attempt_error(error: CodexHandshakeAttemptError) -> MappedProviderFailure {
    match error {
        CodexHandshakeAttemptError::Client(error) => map_handshake_error(error),
        CodexHandshakeAttemptError::Cancelled => MappedProviderFailure::plain(provider_error(
            ProviderErrorKind::Cancelled,
            UpstreamSendState::Ambiguous,
        )),
        CodexHandshakeAttemptError::Timeout => MappedProviderFailure::plain(provider_error(
            ProviderErrorKind::Timeout,
            UpstreamSendState::Ambiguous,
        )),
    }
}

async fn create_json_attempt(
    request: &ColdJsonResponse,
    account: &ProviderAccount,
    installation_id: &str,
    authorization: &SecretString,
    cookie_header: Option<&SecretString>,
    account_selection: CodexAccountSelectionTelemetry<'_>,
) -> Result<CodexBackendJsonResponse, CodexHandshakeAttemptError> {
    let Some(handshake_deadline) = remaining(request.context.deadline()) else {
        return Err(CodexHandshakeAttemptError::Timeout);
    };
    let request_id = request.context.request_id().as_str();
    let mut request_context = CodexRequestContext::auxiliary(
        authorization.expose_secret(),
        account.upstream_account_id(),
        request_id,
        Some(installation_id),
    );
    request_context.cookie_header = cookie_header.map(ExposeSecret::expose_secret);
    request_context.account_selection = account_selection;
    tokio::select! {
        biased;
        _ = request.context.cancellation().cancelled() => Err(CodexHandshakeAttemptError::Cancelled),
        _ = tokio::time::sleep(handshake_deadline) => Err(CodexHandshakeAttemptError::Timeout),
        response = request.client.post_raw_json(
            request.endpoint_path,
            request.body.clone(),
            request.image_turn_id.as_deref(),
            request_context,
        ) => response.map_err(CodexHandshakeAttemptError::Client),
    }
}

fn cold_json_response_stream(request: ColdJsonResponse) -> EventStream {
    Box::pin(async_stream::try_stream! {
        let allows_account_state_mutation = request.lease.allows_account_state_mutation();
        let failure_context = OpenAiFailureContext {
            client: &request.client,
            selector: &request.selector,
            quota: &request.quota,
            response_origin: &request.response_origin,
            cyber_policy_scope: None,
            allows_account_state_mutation,
        };
        let active_account = request.lease.account().clone();
        let cookie_header = build_cookie_header(request.lease.cookies())?;
        let authorization = request
            .lease
            .authentication()
            .authorization_header()
            .map_err(|_| {
                provider_error(
                    ProviderErrorKind::Unauthorized,
                    UpstreamSendState::NotSent,
                )
            })?;
        let account_selection = CodexAccountSelectionTelemetry::new(
            request.lease.affinity_hit(),
            request.lease.escape_reason(),
            request.lease.account_switch(),
        );
        let response = create_json_attempt(
            &request,
            &active_account,
            request.lease.installation_id(),
            &authorization,
            cookie_header.as_ref(),
            account_selection,
        )
        .await;
        let response = match response.map_err(map_handshake_attempt_error) {
            Ok(response) => response,
            Err(mut failure) => {
                if let Some(observation) = failure.observation.take() {
                    yield ProviderEvent::observation(observation);
                }
                apply_failure(&failure_context, &active_account, &failure).await;
                Err(failure.error)?;
                return;
            }
        };

        let mut metrics = response.transport_metrics.clone();
        metrics.first_event_ms = Some(
            i64::try_from(request.output_started_at.elapsed().as_millis()).unwrap_or(i64::MAX),
        );
        if let Some(observation) = codex_response_observation(
            CodexBackendTransport::HttpJson,
            &response.diagnostics,
            &response.response_metadata,
            &metrics,
            None,
            openai_response_timings(&metrics, &response.response_metadata),
        ) {
            yield ProviderEvent::observation(observation);
        }
        if allows_account_state_mutation {
            synchronize_passive_quota_headers(
                &request.quota,
                &active_account,
                &response.rate_limit_headers,
            )
            .await;
            if !response.set_cookie_headers.is_empty()
                && let Err(error) = request
                    .selector
                    .capture_response_cookies(
                        &active_account,
                        &request.response_origin,
                        &response.set_cookie_headers,
                    )
                    .await
            {
                tracing::warn!(
                    account_id = %active_account.id(),
                    error = %error,
                    "Failed to persist OpenAI image response cookies"
                );
            }
        }

        let response_meta =
            ResponseMeta::for_provider_endpoint(request.context.request_id().as_str());
        yield ProviderEvent::canonical(GatewayEvent::Started(response_meta.clone()));
        let wire = ProtocolWireEvent::raw_json(PROVIDER_NAME, response.body).map_err(|_| {
            provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
        })?;
        yield ProviderEvent::wire(wire);
        yield ProviderEvent::canonical(GatewayEvent::Completed(
            response_meta.with_finish_reason(FinishReason::Stop),
        ));
    })
}

fn cold_response_stream(response: ColdResponse) -> EventStream {
    let ColdResponse {
        client,
        response_origin,
        request,
        upstream_model,
        transport_policy,
        context,
        selector,
        quota,
        catalog,
        lease,
        output_started_at,
        session_affinity_key,
        mut session_capture,
    } = response;
    Box::pin(async_stream::try_stream! {
        let cyber_policy_scope = lease.cyber_policy_scope().cloned();
        let allows_account_state_mutation = lease.allows_account_state_mutation();
        let failure_context = OpenAiFailureContext {
            client: &client,
            selector: &selector,
            quota: &quota,
            response_origin: &response_origin,
            cyber_policy_scope: cyber_policy_scope.as_ref(),
            allows_account_state_mutation,
        };
        let mut active_account = lease.account().clone();
        let cookie_header = build_cookie_header(lease.cookies())?;
        let authorization = lease
            .authentication()
            .authorization_header()
            .map_err(|_| {
                provider_error(
                    ProviderErrorKind::Unauthorized,
                    UpstreamSendState::NotSent,
                )
            })?;
        let request_id = context.request_id().as_str().to_owned();
        let cancellation = context.cancellation().clone();
        let account_selection = CodexAccountSelectionTelemetry::new(
            lease.affinity_hit(),
            lease.escape_reason(),
            lease.account_switch(),
        );
        let response = create_response_attempt(
            &client,
            &request,
            codex_request_context(
                &request,
                &request_id,
                &active_account,
                lease.installation_id(),
                &authorization,
                cookie_header.as_ref(),
                account_selection,
            ),
            active_account.id().as_str(),
            context.deadline(),
            &cancellation,
        )
        .await;
        let response = response.map_err(map_handshake_attempt_error);
        let response = match response {
            Ok(response) => response,
            Err(mut failure) => {
                if let Some(observation) = failure.observation.take() {
                    yield ProviderEvent::observation(observation);
                }
                apply_failure(&failure_context, &active_account, &failure)
                .await;
                Err(failure.error)?;
                return;
            }
        };
        if !accepts_backend_transport(transport_policy, response.transport) {
            let failure = MappedProviderFailure::plain(provider_error(
                ProviderErrorKind::Protocol,
                UpstreamSendState::Sent,
            ));
            apply_failure(&failure_context, &active_account, &failure)
            .await;
            Err(failure.error)?;
            return;
        }
        if let Some(capture) = session_capture.as_mut() {
            capture.continuation_scope = Some(if capture.response_store {
                OpenAiContinuationScope::Persisted
            } else if response.transport == CodexBackendTransport::WebSocket
                && response.connection_local_continuation
            {
                OpenAiContinuationScope::ConnectionLocal
            } else {
                OpenAiContinuationScope::ReplayRequired
            });
            capture.turn_state = response.turn_state.clone().or(capture.turn_state.clone());
        }
        let mut observation_state = OpenAiResponseObservationState::from_backend_response(
            &response,
            &request,
            active_account.id().as_str(),
            context.attempt_index().get(),
        );
        if let Some(observation) = observation_state.observation() {
            yield ProviderEvent::observation(observation);
        }
        if let Some(etag) = response.response_metadata.models_etag.as_deref()
            && let Err(error) = catalog.observe_response_etag(etag)
        {
            tracing::warn!(
                error = %error,
                "OpenAI model ETag observation was rejected"
            );
        }
        if allows_account_state_mutation
            && !response.set_cookie_headers.is_empty()
            && let Ok(outcome) = selector
                .capture_response_cookies(
                    &active_account,
                    &response_origin,
                    &response.set_cookie_headers,
                )
                .await
                && let Some(revision) = outcome.credential_revision
                && let Ok(current) = selector.current_account(active_account.id()).await
                && current.revision().get() == revision
        {
            active_account = current;
        }
        let mut body = response.body;
        let failure_diagnostics = response.diagnostics.clone();
        let failure_set_cookie_headers = response.set_cookie_headers.clone();
        let failure_rate_limit_headers = response.rate_limit_headers.clone();
        let mut passive_quota_observation =
            OpenAiPassiveQuotaObservation::new(response.rate_limit_headers);
        let rate_limit_updates = response.rate_limit_updates;
        let turn_state_updates = response.turn_state_update;
        // OpenAI 线路为透明代理：HTTP SSE 与 WebSocket 两条上游均启用 raw 透传，
        // 下游按字节转发上游原文，避免 serde 往返改写数值/精度（大整数→f64、logprobs 等）。
        // WS 帧由 reducer 以 encode_sse_event(&event, raw) 逐字节内嵌上游原始 JSON
        // （transport/protocol/websocket.rs），push_frames 抽出的 data 即上游原文。
        let mut decoder = CodexCanonicalDecoder::new(upstream_model.as_str())
            .with_requested_service_tier(request.service_tier())
            .with_request_tool_pricing(upstream_model.as_str(), request.tools())
            .with_raw_sse_passthrough();
        let mut pre_commit_events = PreCommitClientEvents::new();
        loop {
            let Some(stream_deadline) = remaining(context.deadline()) else {
                if allows_account_state_mutation {
                    synchronize_passive_quota(
                        &quota,
                        &active_account,
                        passive_quota_observation.rate_limits(),
                    )
                    .await;
                }
                Err(provider_error(ProviderErrorKind::Timeout, UpstreamSendState::Sent))?;
                return;
            };
            let replay_grace_deadline = pre_commit_events.replay_grace_deadline();
            let next = tokio::select! {
                biased;
                _ = cancellation.cancelled() => Err(MappedProviderFailure::plain(provider_error(
                    ProviderErrorKind::Cancelled,
                    UpstreamSendState::Sent,
                ))),
                _ = tokio::time::sleep(stream_deadline) => Err(MappedProviderFailure::plain(provider_error(
                    ProviderErrorKind::Timeout,
                    UpstreamSendState::Sent,
                ))),
                _ = wait_for_replay_grace(replay_grace_deadline) => Ok(PreCommitPoll::GraceElapsed),
                chunk = body.next() => match chunk {
                    Some(Ok(chunk)) => Ok(PreCommitPoll::Upstream(Some(chunk))),
                    Some(Err(error)) => Err(map_stream_error(error)),
                    None => Ok(PreCommitPoll::Upstream(None)),
                },
            };
            let next = match next {
                Ok(PreCommitPoll::Upstream(next)) => next,
                Ok(PreCommitPoll::GraceElapsed) => {
                    for event in pre_commit_events.commit_pending() {
                        yield event;
                    }
                    continue;
                }
                Err(failure) => {
                    let updates = take_rate_limit_updates(rate_limit_updates.as_ref()).await;
                    if !updates.is_empty() {
                        passive_quota_observation.observe(&updates);
                        let update_headers = rate_limit_update_headers(&updates);
                        if observation_state.merge_rate_limit_headers(&update_headers)
                            && let Some(observation) = observation_state.observation()
                        {
                            yield ProviderEvent::observation(observation);
                        }
                    }
                    if allows_account_state_mutation {
                        synchronize_passive_quota(
                            &quota,
                            &active_account,
                            passive_quota_observation.rate_limits(),
                        )
                        .await;
                    }
                    apply_failure(&failure_context, &active_account, &failure)
                    .await;
                    Err(failure.error)?;
                    return;
                }
            };
            let Some(chunk) = next else { break; };
            let updates = take_rate_limit_updates(rate_limit_updates.as_ref()).await;
            let rate_limits_changed = if updates.is_empty() {
                false
            } else {
                passive_quota_observation.observe(&updates);
                observation_state.merge_rate_limit_headers(&rate_limit_update_headers(&updates))
            };
            let turn_state_changed = if let Some(updates) = turn_state_updates.as_ref()
                && let Some(turn_state) = updates.lock().await.take()
            {
                if let Some(capture) = session_capture.as_mut() {
                    capture.turn_state = Some(turn_state.clone());
                }
                observation_state.merge_client_header("x-codex-turn-state", &turn_state)
            } else {
                false
            };
            let first_event_changed =
                observation_state.observe_stream_chunk(&chunk, output_started_at);
            let chunk_len = chunk.len();
            let (mut events, canonical_failure) = match decoder.push(&chunk) {
                CodexCanonicalOutcome::Events(events) => (events, None),
                CodexCanonicalOutcome::Failed(failure) => {
                    let (events, error, semantic_output_seen) = failure.into_parts();
                    (events, Some((error, semantic_output_seen)))
                }
            };
            pre_commit_events.observe_chunk(chunk_len);
            let service_tier_changed = observation_state
                .observe_upstream_service_tier(decoder.response_service_tier());
            let terminal_failure = canonical_failure.map(|(error, semantic_output_seen)| {
                let atomic_upstream_failure = matches!(&error, CodexCanonicalError::Upstream(_));
                (
                    map_canonical_error(
                        error,
                        &failure_diagnostics,
                        &failure_set_cookie_headers,
                        &failure_rate_limit_headers,
                        ReplayBoundary::from_semantic_output(
                            semantic_output_seen || pre_commit_events.is_committed(),
                        ),
                    ),
                    atomic_upstream_failure,
                )
            });
            let timing_signals = decoder.take_timing_signals();
            let timing_changed = first_event_changed
                || observation_state
                    .observe_timing_signals(timing_signals, output_started_at);
            let completed = events
                .iter()
                .flat_map(ProviderEvent::canonical_facts)
                .any(|event| matches!(event, GatewayEvent::Completed(_)));
            let terminal_changed = completed
                && observation_state.mark_completed(terminal_response_is_incomplete(&events));
            if allows_account_state_mutation && (completed || terminal_failure.is_some()) {
                synchronize_passive_quota(
                    &quota,
                    &active_account,
                    passive_quota_observation.rate_limits(),
                )
                .await;
            }
            if let Some((failure, _)) = terminal_failure.as_ref() {
                apply_failure(&failure_context, &active_account, failure)
                .await;
            }
            attach_openai_session_update(&mut events, &mut session_capture);
            if allows_account_state_mutation && completed && terminal_failure.is_none() {
                // 完成事件一旦交给下游，Core 可以立刻停止轮询 Provider stream；
                // 在此之前持久化亲和关系，保证成功请求不会因流被提前 drop 而丢失绑定。
                selector
                    .record_success(
                        &active_account,
                        session_affinity_key.as_ref(),
                        lease.affinity_expected_account_id(),
                    )
                    .await;
                selector
                    .observe_cyber_policy_success(cyber_policy_scope.as_ref())
                    .await;
            }
            if (rate_limits_changed
                || service_tier_changed
                || timing_changed
                || turn_state_changed
                || terminal_changed)
                && let Some(observation) = observation_state.observation()
            {
                yield ProviderEvent::observation(observation);
            }
            if let Some((mut failure, atomic_upstream_failure)) = terminal_failure {
                let failure_after_commit =
                    timing_signals.semantic_output || pre_commit_events.is_committed();
                if failure_after_commit {
                    for event in pre_commit_events.commit(events) {
                        yield event;
                    }
                } else if atomic_upstream_failure {
                    failure.error = failure
                        .error
                        .with_atomic_client_events(pre_commit_events.take_for_failure(events));
                }
                Err(failure.error)?;
                return;
            }
            let events = pre_commit_events.stage(events, timing_signals, completed);
            for event in events {
                yield event;
            }
            if completed {
                return;
            }
        }
        let (mut events, canonical_failure) = match decoder.finish() {
            CodexCanonicalOutcome::Events(events) => (events, None),
            CodexCanonicalOutcome::Failed(failure) => {
                let (events, error, semantic_output_seen) = failure.into_parts();
                (events, Some((error, semantic_output_seen)))
            }
        };
        let terminal_failure = canonical_failure.map(|(error, semantic_output_seen)| {
            let atomic_upstream_failure = matches!(&error, CodexCanonicalError::Upstream(_));
            (
                map_canonical_error(
                    error,
                    &failure_diagnostics,
                    &failure_set_cookie_headers,
                    &failure_rate_limit_headers,
                    ReplayBoundary::from_semantic_output(
                        semantic_output_seen || pre_commit_events.is_committed(),
                    ),
                ),
                atomic_upstream_failure,
            )
        });
        let timing_signals = decoder.take_timing_signals();
        let service_tier_changed = observation_state
            .observe_upstream_service_tier(decoder.response_service_tier());
        let timing_changed = observation_state
            .observe_timing_signals(timing_signals, output_started_at);
        let updates = take_rate_limit_updates(rate_limit_updates.as_ref()).await;
        let rate_limits_changed = if updates.is_empty() {
            false
        } else {
            passive_quota_observation.observe(&updates);
            observation_state.merge_rate_limit_headers(&rate_limit_update_headers(&updates))
        };
        if allows_account_state_mutation {
            synchronize_passive_quota(
                &quota,
                &active_account,
                passive_quota_observation.rate_limits(),
            )
            .await;
        }
        if let Some((failure, _)) = terminal_failure.as_ref() {
            apply_failure(&failure_context, &active_account, failure)
            .await;
        }
        let turn_state_changed = if let Some(updates) = turn_state_updates.as_ref()
            && let Some(turn_state) = updates.lock().await.take()
        {
            if let Some(capture) = session_capture.as_mut() {
                capture.turn_state = Some(turn_state.clone());
            }
            observation_state.merge_client_header("x-codex-turn-state", &turn_state)
        } else {
            false
        };
        attach_openai_session_update(&mut events, &mut session_capture);
        let completed = events
            .iter()
            .flat_map(ProviderEvent::canonical_facts)
            .any(|event| matches!(event, GatewayEvent::Completed(_)));
        let terminal_changed = completed
            && observation_state.mark_completed(terminal_response_is_incomplete(&events));
        if allows_account_state_mutation && completed && terminal_failure.is_none() {
            // 同上：尾部 finish() 也可能产出 completed，亲和记录必须先于任何下游 yield。
            selector
                .record_success(
                    &active_account,
                    session_affinity_key.as_ref(),
                    lease.affinity_expected_account_id(),
                )
                .await;
            selector
                .observe_cyber_policy_success(cyber_policy_scope.as_ref())
                .await;
        }
        if (service_tier_changed
            || timing_changed
            || rate_limits_changed
            || turn_state_changed
            || terminal_changed)
            && let Some(observation) = observation_state.observation()
        {
            yield ProviderEvent::observation(observation);
        }
        if let Some((mut failure, atomic_upstream_failure)) = terminal_failure {
            let failure_after_commit =
                timing_signals.semantic_output || pre_commit_events.is_committed();
            if failure_after_commit {
                for event in pre_commit_events.commit(events) {
                    yield event;
                }
            } else if atomic_upstream_failure {
                failure.error = failure
                    .error
                    .with_atomic_client_events(pre_commit_events.take_for_failure(events));
            }
            Err(failure.error)?;
            return;
        }
        let events = pre_commit_events.finish(events, timing_signals, completed);
        for event in events {
            yield event;
        }
    })
}

/// OpenAI Responses 的观测状态完全归 Provider 所有。
///
/// 每次原始 SSE/WS 事件推进状态后重新生成不可变 observation，Core 只负责携带、
/// 持久化和展示这份安全快照，不解释任何 OpenAI 协议字段。
struct OpenAiResponseObservationState {
    transport: CodexBackendTransport,
    diagnostics: CodexUpstreamDiagnostics,
    response_metadata: CodexResponseMetadata,
    metrics: CodexTransportMetrics,
    websocket_pool_decision: Option<crate::transport::WebSocketPoolDecision>,
    request_summary: Value,
    requested_model: String,
    stream: bool,
    compact: bool,
    requested_service_tier: Option<String>,
    upstream_service_tier: Option<String>,
    rate_limit_headers: Vec<(String, String)>,
    account_id: String,
    attempt_index: u32,
    timings: ProviderResponseTimings,
    terminal: Option<OpenAiResponseTerminal>,
}

struct OpenAiPassiveQuotaObservation {
    rate_limits: Vec<ParsedRateLimits>,
}

impl OpenAiPassiveQuotaObservation {
    fn new(headers: Vec<(String, String)>) -> Self {
        Self {
            rate_limits: parse_rate_limit_headers(&headers).into_iter().collect(),
        }
    }

    fn observe(&mut self, updates: &[ParsedRateLimits]) {
        self.rate_limits.extend_from_slice(updates);
    }

    fn rate_limits(&self) -> &[ParsedRateLimits] {
        &self.rate_limits
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenAiResponseTerminal {
    Completed,
    Incomplete,
}

impl OpenAiResponseObservationState {
    fn from_backend_response(
        response: &CodexBackendStreamingResponse,
        request: &CodexResponsesRequest,
        account_id: &str,
        attempt_index: u32,
    ) -> Self {
        let semantics = request.semantics();
        Self {
            transport: response.transport,
            diagnostics: response.diagnostics.clone(),
            response_metadata: response.response_metadata.clone(),
            metrics: response.transport_metrics.clone(),
            websocket_pool_decision: response.websocket_pool_decision,
            request_summary: openai_response_request_summary(request, response.transport),
            requested_model: request.model().to_owned(),
            stream: request.stream(),
            compact: semantics.compact,
            requested_service_tier: normalize_service_tier(request.service_tier()),
            upstream_service_tier: None,
            rate_limit_headers: selected_observation_headers(&response.rate_limit_headers),
            account_id: account_id.to_owned(),
            attempt_index,
            timings: openai_response_timings(
                &response.transport_metrics,
                &response.response_metadata,
            ),
            terminal: None,
        }
    }

    fn observation(&self) -> Option<ProviderResponseObservation> {
        let mut observation = codex_response_observation(
            self.transport,
            &self.diagnostics,
            &self.response_metadata,
            &self.metrics,
            self.websocket_pool_decision,
            self.timings,
        )?;
        if let Some(metadata) = self.provider_metadata() {
            observation = observation.with_provider_metadata(metadata);
        }
        if let Some(service_tier) = self.effective_service_tier() {
            observation = observation.with_service_tier_if_valid(service_tier.to_owned());
        }
        Some(observation)
    }

    fn observe_stream_chunk(&mut self, chunk: &[u8], started_at: Instant) -> bool {
        if chunk.is_empty() {
            return false;
        }
        insert_first_timing(&mut self.timings.first_event_ms, started_at)
    }

    fn observe_timing_signals(
        &mut self,
        signals: ResponseEventSignals,
        started_at: Instant,
    ) -> bool {
        let mut changed = false;
        // 首个非前导输出事件（结构帧也算）开启首字计时；
        // 真实语义首字由 first_reasoning_ms / first_text_ms 单独观测。
        if signals.output_start {
            changed |= insert_first_timing(&mut self.timings.first_token_ms, started_at);
        }
        if signals.reasoning_output {
            changed |= insert_first_timing(&mut self.timings.first_reasoning_ms, started_at);
        }
        if signals.text_output {
            changed |= insert_first_timing(&mut self.timings.first_text_ms, started_at);
        }
        changed
    }

    fn observe_upstream_service_tier(&mut self, service_tier: Option<&str>) -> bool {
        let Some(service_tier) = normalize_service_tier(service_tier) else {
            return false;
        };
        if self.upstream_service_tier.as_deref() == Some(service_tier.as_str()) {
            return false;
        }
        self.upstream_service_tier = Some(service_tier);
        true
    }

    fn effective_service_tier(&self) -> Option<&str> {
        self.requested_service_tier
            .as_deref()
            .or(self.upstream_service_tier.as_deref())
    }

    fn merge_rate_limit_headers(&mut self, updates: &[(String, String)]) -> bool {
        let mut changed = false;
        for (name, value) in selected_observation_headers(updates) {
            if let Some(existing) = self
                .rate_limit_headers
                .iter_mut()
                .find(|(existing_name, _)| existing_name == &name)
            {
                if existing.1 != value {
                    existing.1 = value;
                    changed = true;
                }
            } else {
                self.rate_limit_headers.push((name, value));
                changed = true;
            }
        }
        changed
    }

    fn merge_client_header(&mut self, name: &str, value: &str) -> bool {
        let value = Bytes::copy_from_slice(value.as_bytes());
        if let Some((_, existing)) = self
            .response_metadata
            .client_headers
            .iter_mut()
            .find(|(existing_name, _)| existing_name.eq_ignore_ascii_case(name))
        {
            if *existing == value {
                return false;
            }
            *existing = value;
            return true;
        }
        self.response_metadata
            .client_headers
            .push((name.to_owned(), value));
        true
    }

    fn mark_completed(&mut self, incomplete: bool) -> bool {
        let terminal = if incomplete {
            OpenAiResponseTerminal::Incomplete
        } else {
            OpenAiResponseTerminal::Completed
        };
        if self.terminal == Some(terminal) {
            return false;
        }
        self.terminal = Some(terminal);
        true
    }

    fn provider_metadata(&self) -> Option<ProviderResponseMetadata> {
        let mut metadata = Map::new();
        // 观测对象版本信封：读取端按 schemaVersion 解码，缺失视为 v0。
        metadata.insert("schemaVersion".to_owned(), json!(1));
        let effective_model = self
            .response_metadata
            .effective_model
            .as_deref()
            .filter(|model| !model.is_empty())
            .unwrap_or(&self.requested_model);
        if !effective_model.is_empty() {
            metadata.insert(
                "effectiveModel".to_owned(),
                Value::String(effective_model.to_owned()),
            );
        }
        metadata.insert(
            "modelsEtag".to_owned(),
            self.response_metadata
                .models_etag
                .as_ref()
                .map_or(Value::Null, |etag| Value::String(etag.clone())),
        );
        metadata.insert(
            "reasoningIncluded".to_owned(),
            Value::Bool(self.response_metadata.reasoning_included),
        );
        metadata.insert("stream".to_owned(), Value::Bool(self.stream));
        metadata.insert("compact".to_owned(), Value::Bool(self.compact));
        metadata.insert("attemptIndex".to_owned(), json!(self.attempt_index));
        metadata.insert(
            "attemptAccountId".to_owned(),
            Value::String(self.account_id.clone()),
        );
        metadata.insert(
            "rateLimitHeaders".to_owned(),
            json!(self.rate_limit_headers),
        );
        metadata.insert(
            "upstreamTraceHeaders".to_owned(),
            json!(selected_observation_headers(
                &self.diagnostics.trace_headers
            )),
        );
        metadata.insert("requestSummary".to_owned(), self.request_summary.clone());
        if let Some(service_tier) = self.effective_service_tier() {
            metadata.insert(
                "serviceTier".to_owned(),
                Value::String(service_tier.to_owned()),
            );
        }
        if let Some(service_tier) = &self.requested_service_tier {
            metadata.insert(
                "requestedServiceTier".to_owned(),
                Value::String(service_tier.clone()),
            );
        }
        if let Some(service_tier) = &self.upstream_service_tier {
            metadata.insert(
                "upstreamServiceTier".to_owned(),
                Value::String(service_tier.clone()),
            );
        }
        if let Some(status) = self.diagnostics.status_code {
            metadata.insert("upstreamStatus".to_owned(), json!(status));
        }
        if let Some(decision) = self.metrics.decision {
            metadata.insert(
                "transportDecision".to_owned(),
                Value::String(decision.as_str().to_owned()),
            );
        }
        if let Some(decision) = self.websocket_pool_decision {
            metadata.insert(
                "websocketPool".to_owned(),
                json!({ "kind": decision.kind() }),
            );
        }
        if let Some(version) = self.metrics.http_version.as_deref() {
            metadata.insert("httpVersion".to_owned(), Value::String(version.to_owned()));
        }
        if let Some((_, cf_ray)) = self
            .diagnostics
            .trace_headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("cf-ray"))
            && let Some((_, cf_ray)) = selected_observation_header("cf-ray", cf_ray)
        {
            metadata.insert("cfRay".to_owned(), Value::String(cf_ray));
        }
        insert_optional_metadata_millis(
            &mut metadata,
            "transportDecisionWaitMs",
            self.timings.transport_decision_wait_ms,
        );
        insert_optional_metadata_millis(&mut metadata, "wsConnectMs", self.timings.connect_ms);
        insert_optional_metadata_millis(
            &mut metadata,
            "upstreamHeadersMs",
            self.timings.headers_ms,
        );
        insert_optional_metadata_millis(&mut metadata, "firstEventMs", self.timings.first_event_ms);
        insert_optional_metadata_millis(
            &mut metadata,
            "firstReasoningMs",
            self.timings.first_reasoning_ms,
        );
        insert_optional_metadata_millis(&mut metadata, "firstTextMs", self.timings.first_text_ms);
        insert_optional_metadata_millis(&mut metadata, "firstTokenMs", self.timings.first_token_ms);
        insert_optional_metadata_millis(
            &mut metadata,
            "openaiProcessingMs",
            self.timings.provider_processing_ms,
        );
        if let Some(terminal) = self.terminal {
            let incomplete = terminal == OpenAiResponseTerminal::Incomplete;
            metadata.insert("completed".to_owned(), Value::Bool(!incomplete));
            metadata.insert("incomplete".to_owned(), Value::Bool(incomplete));
            metadata.insert("eventStatusCode".to_owned(), json!(200_u16));
        }
        ProviderResponseMetadata::new(serde_json::to_string(&Value::Object(metadata)).ok()?)
    }
}

fn codex_response_observation(
    transport: CodexBackendTransport,
    diagnostics: &CodexUpstreamDiagnostics,
    response_metadata: &CodexResponseMetadata,
    metrics: &CodexTransportMetrics,
    websocket_pool_decision: Option<crate::transport::WebSocketPoolDecision>,
    timings: ProviderResponseTimings,
) -> Option<ProviderResponseObservation> {
    let mut observation = ProviderResponseObservation::new(
        UpstreamTransport::new(actual_transport_name(transport)).ok()?,
    )
    .with_timings(timings);
    if let Some(version) = metrics
        .http_version
        .as_deref()
        .and_then(UpstreamHttpVersion::parse)
    {
        observation = observation.with_http_version(version);
    }
    if let Some(decision) = websocket_pool_decision {
        observation = observation.with_websocket_pool(if decision.is_reuse() {
            WebSocketPoolKind::Reuse
        } else {
            WebSocketPoolKind::New
        });
    }
    if let Some(status_code) = diagnostics.status_code {
        observation = observation.with_status_code(status_code);
    }
    if let Some(request_id) = diagnostics.request_id.as_deref() {
        observation = observation.with_request_id(OpaqueUpstreamValue::new(request_id.to_owned()));
    }
    let client_headers = response_metadata
        .client_headers
        .iter()
        .map(|(name, value)| ProviderResponseHeader::new(name.to_owned(), value.clone()))
        .collect();
    observation = observation.with_client_headers(client_headers);
    Some(observation)
}

fn codex_error_observation(error: &CodexClientError) -> Option<ProviderResponseObservation> {
    let transport = error.transport()?;
    let mut observation = ProviderResponseObservation::new(
        UpstreamTransport::new(actual_transport_name(transport)).ok()?,
    );
    match error {
        CodexClientError::Upstream {
            status,
            diagnostics,
            transport,
            transport_metrics,
            ..
        } => {
            observation = codex_response_observation(
                *transport,
                diagnostics,
                &CodexResponseMetadata::default(),
                transport_metrics,
                None,
                openai_response_timings(transport_metrics, &CodexResponseMetadata::default()),
            )?
            .with_status_code(status.as_u16());
        }
        CodexClientError::WebSocket(CodexWebSocketExchangeError::Upstream(upstream)) => {
            observation = observation.with_status_code(upstream.status_code);
            if let Some(request_id) = upstream.diagnostics.request_id.as_deref() {
                observation =
                    observation.with_request_id(OpaqueUpstreamValue::new(request_id.to_owned()));
            }
        }
        _ => {}
    }
    Some(observation)
}

fn openai_response_timings(
    metrics: &CodexTransportMetrics,
    response_metadata: &CodexResponseMetadata,
) -> ProviderResponseTimings {
    ProviderResponseTimings {
        transport_decision_wait_ms: nonnegative_millis(metrics.transport_decision_wait_ms),
        connect_ms: nonnegative_millis(metrics.ws_connect_ms),
        headers_ms: nonnegative_millis(metrics.upstream_headers_ms),
        first_event_ms: nonnegative_millis(metrics.first_event_ms),
        provider_processing_ms: openai_processing_ms(response_metadata),
        ..ProviderResponseTimings::default()
    }
}

fn openai_processing_ms(response_metadata: &CodexResponseMetadata) -> Option<u64> {
    response_metadata
        .client_headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("openai-processing-ms"))
        .and_then(|(_, value)| std::str::from_utf8(value).ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
}

fn openai_response_request_summary(
    request: &CodexResponsesRequest,
    transport: CodexBackendTransport,
) -> Value {
    let body = request.body();
    let input = body.get("input");
    let tools = body.get("tools");
    let semantics = request.semantics();
    json!({
        "model": request.model(),
        "stream": request.stream(),
        "store": request.store(),
        "compact": semantics.compact,
        "requestKind": semantics.request_kind,
        "subagentKind": semantics.subagent_kind,
        "reasoningPreset": semantics.reasoning_preset,
        "transport": actual_transport_name(transport),
        "inputType": json_value_kind(input),
        "inputItemsCount": input.and_then(Value::as_array).map(Vec::len),
        "toolsType": json_value_kind(tools),
        "toolsCount": tools.and_then(Value::as_array).map(Vec::len),
        "topLevelFields": body.keys().cloned().collect::<Vec<_>>(),
        "previousResponseIdPresent": request.previous_response_id().is_some(),
        "serviceTier": request.service_tier(),
        "localTransport": {
            "useWebsocket": request.use_websocket,
            "forceHttpSse": request.force_http_sse,
        },
    })
}

const fn json_value_kind(value: Option<&Value>) -> &'static str {
    match value {
        None => "missing",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

fn selected_observation_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| selected_observation_header(name, value))
        .collect()
}

fn selected_observation_header(name: &str, value: &str) -> Option<(String, String)> {
    let name = name.trim().to_ascii_lowercase();
    (!name.is_empty()).then(|| (name, value.to_owned()))
}

fn insert_first_timing(target: &mut Option<u64>, started_at: Instant) -> bool {
    if target.is_some() {
        return false;
    }
    let elapsed = u64::try_from(started_at.elapsed().as_millis())
        .unwrap_or(u64::MAX)
        .max(1);
    *target = Some(elapsed);
    true
}

fn insert_optional_metadata_millis(
    metadata: &mut Map<String, Value>,
    name: &str,
    value: Option<u64>,
) {
    if let Some(value) = value {
        metadata.insert(name.to_owned(), json!(value));
    }
}

async fn take_rate_limit_updates(updates: Option<&CodexRateLimitUpdates>) -> Vec<ParsedRateLimits> {
    let Some(updates) = updates else {
        return Vec::new();
    };
    std::mem::take(&mut *updates.lock().await)
}

fn rate_limit_update_headers(updates: &[ParsedRateLimits]) -> Vec<(String, String)> {
    updates
        .iter()
        .flat_map(rate_limits_to_header_pairs)
        .collect()
}

fn terminal_response_is_incomplete(events: &[ProviderEvent]) -> bool {
    events
        .iter()
        .find_map(|event| {
            let wire = event.wire_event()?;
            let event_type = wire
                .event_type()
                .or_else(|| wire.data().get("type").and_then(Value::as_str));
            match event_type {
                Some("response.incomplete") => Some(true),
                Some("response.completed") => Some(
                    wire.data()
                        .pointer("/response/status")
                        .and_then(Value::as_str)
                        == Some("incomplete"),
                ),
                _ => None,
            }
        })
        .unwrap_or(false)
}

async fn synchronize_passive_quota(
    quota: &CodexCredentialQuotaService,
    account: &ProviderAccount,
    rate_limits: &[ParsedRateLimits],
) {
    if rate_limits.is_empty() {
        return;
    }
    if let Err(error) = quota
        .synchronize_passive_rate_limits(account, rate_limits)
        .await
    {
        tracing::warn!(
            account_id = %account.id(),
            error = %error,
            "OpenAI passive quota synchronization failed"
        );
    }
}

async fn synchronize_passive_quota_headers(
    quota: &CodexCredentialQuotaService,
    account: &ProviderAccount,
    headers: &[(String, String)],
) {
    let Some(rate_limits) = parse_rate_limit_headers(headers) else {
        return;
    };
    synchronize_passive_quota(quota, account, std::slice::from_ref(&rate_limits)).await;
}

const fn actual_transport_name(transport: CodexBackendTransport) -> &'static str {
    match transport {
        CodexBackendTransport::HttpSse => HTTP_SSE_TRANSPORT,
        CodexBackendTransport::HttpJson => HTTP_JSON_TRANSPORT,
        CodexBackendTransport::WebSocket => WEBSOCKET_TRANSPORT,
    }
}

fn nonnegative_millis(value: Option<i64>) -> Option<u64> {
    value.and_then(|value| u64::try_from(value).ok())
}

fn compile_model_capabilities(model: &CodexCatalogModel) -> ProviderModelCapabilities {
    // Catalog membership is enough to publish Generate. `supported_in_api` is advisory;
    // the upstream response is authoritative for normal and diagnostic requests.
    let capabilities = ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), None)
        .with_upstream_feature_validation();
    ProviderModelCapabilities::new(model.request_model().clone(), capabilities)
        .with_presentation(codex_model_presentation(model))
}

fn codex_model_presentation(model: &CodexCatalogModel) -> ModelPresentation {
    let capabilities = model.capabilities();
    let reasoning_efforts = capabilities.reasoning_efforts().to_vec();
    // Codex 目录不声明默认 effort；有 medium 时对齐官方 picker 缺省，否则取首项。
    let default_reasoning = reasoning_efforts
        .iter()
        .find(|effort| effort.as_str() == "medium")
        .or_else(|| reasoning_efforts.first())
        .cloned();
    let hidden = matches!(
        model.metadata().visibility(),
        Some(CodexCatalogVisibility::Hide | CodexCatalogVisibility::None)
    );

    ModelPresentation::new(
        Some(model.display_name().to_owned()),
        model.metadata().description().map(str::to_owned),
    )
    .with_reasoning(default_reasoning, reasoning_efforts)
    .with_context_window_tokens(
        model
            .limits()
            .context_window_tokens()
            .map(std::num::NonZeroU64::get),
    )
    .with_image_input(capabilities.image_input() == CodexCatalogCapabilityEvidence::DeclaredNative)
    // Codex Responses 工具协议随 API 支持一并可用；只有明确不支持才关闭。
    .with_agent_tools(
        capabilities.responses_api() != CodexCatalogCapabilityEvidence::DeclaredUnsupported,
        capabilities.parallel_tool_calls() == CodexCatalogCapabilityEvidence::DeclaredNative,
    )
    .with_search_tool(capabilities.web_search() == CodexCatalogCapabilityEvidence::DeclaredNative)
    .with_image_detail_original(
        capabilities.image_detail_original() == CodexCatalogCapabilityEvidence::DeclaredNative,
    )
    .with_verbosity(capabilities.verbosity() == CodexCatalogCapabilityEvidence::DeclaredNative)
    .with_hidden(hidden)
}

fn selected_transport(request: &CodexResponsesRequest) -> CodexProviderTransport {
    if request.force_http_sse {
        CodexProviderTransport::HttpOnly
    } else {
        CodexProviderTransport::PreferWebSocket
    }
}

fn apply_transport(request: &mut CodexResponsesRequest, transport: CodexProviderTransport) {
    match transport {
        CodexProviderTransport::HttpOnly => {
            request.force_http_sse = true;
            request.use_websocket = false;
        }
        CodexProviderTransport::PreferWebSocket => {
            request.force_http_sse = false;
            request.use_websocket = true;
        }
    }
}

const fn transport_name(transport: CodexProviderTransport) -> &'static str {
    match transport {
        CodexProviderTransport::HttpOnly => HTTP_SSE_TRANSPORT,
        CodexProviderTransport::PreferWebSocket => WEBSOCKET_TRANSPORT,
    }
}

const fn accepts_backend_transport(
    transport: CodexProviderTransport,
    actual: CodexBackendTransport,
) -> bool {
    match transport {
        CodexProviderTransport::HttpOnly => matches!(actual, CodexBackendTransport::HttpSse),
        CodexProviderTransport::PreferWebSocket => true,
    }
}

fn codex_request_context<'a>(
    request: &'a CodexResponsesRequest,
    request_id: &'a str,
    account: &'a ProviderAccount,
    installation_id: &'a str,
    authorization: &'a SecretString,
    cookie_header: Option<&'a SecretString>,
    account_selection: CodexAccountSelectionTelemetry<'a>,
) -> CodexRequestContext<'a> {
    CodexRequestContext {
        authorization: authorization.expose_secret(),
        account_id: account.upstream_account_id(),
        request_id,
        turn_state: request.turn_state.as_deref(),
        turn_metadata: request.turn_metadata.as_deref(),
        beta_features: request.beta_features.as_deref(),
        include_timing_metrics: request.include_timing_metrics.as_deref(),
        version: request.version.as_deref(),
        codex_window_id: request.codex_window_id.as_deref(),
        parent_thread_id: request.parent_thread_id.as_deref(),
        cookie_header: cookie_header.map(ExposeSecret::expose_secret),
        installation_id: Some(installation_id),
        session_id: request.client_session_id.as_deref(),
        thread_id: request.client_thread_id.as_deref(),
        client_request_id: request.client_request_id.as_deref(),
        turn_id: request.client_turn_id.as_deref(),
        account_selection,
    }
}

fn build_cookie_header(
    cookies: &[RuntimeCodexCookie],
) -> Result<Option<SecretString>, ProviderError> {
    if cookies.is_empty() {
        return Ok(None);
    }
    let mut header = String::new();
    for cookie in cookies {
        let value = cookie.value.expose_secret();
        if !valid_cookie_name(&cookie.name)
            || value.is_empty()
            || value.chars().any(char::is_control)
            || value.contains(';')
        {
            return Err(provider_error(
                ProviderErrorKind::Protocol,
                UpstreamSendState::NotSent,
            ));
        }
        if !header.is_empty() {
            header.push_str("; ");
        }
        header.push_str(&cookie.name);
        header.push('=');
        header.push_str(value);
        if header.len() > MAX_COOKIE_HEADER_BYTES {
            return Err(provider_error(
                ProviderErrorKind::Protocol,
                UpstreamSendState::NotSent,
            ));
        }
    }
    Ok(Some(SecretString::from(header)))
}

fn valid_cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn map_request_error(error: CodexRequestEncodeError) -> ProviderError {
    let kind = match error {
        CodexRequestEncodeError::InvalidProtocolPayload => ProviderErrorKind::InvalidRequest,
    };
    provider_error(kind, UpstreamSendState::NotSent)
}

fn map_selection_error(error: CredentialSelectionError) -> ProviderError {
    match error {
        CredentialSelectionError::CapacityUnavailable { retry_after } => {
            let error = provider_error(
                ProviderErrorKind::AccountCapacityUnavailable,
                UpstreamSendState::NotSent,
            );
            match retry_after {
                Some(retry) => error.with_retry_after(retry),
                None => error,
            }
        }
        CredentialSelectionError::NoEligibleCredential => provider_error(
            ProviderErrorKind::NoEligibleAccount,
            UpstreamSendState::NotSent,
        ),
        CredentialSelectionError::InvalidCredential
        | CredentialSelectionError::Store
        | CredentialSelectionError::Coordinator
        | CredentialSelectionError::CookiePolicy => provider_error(
            ProviderErrorKind::ProviderInfrastructureUnavailable,
            UpstreamSendState::NotSent,
        ),
    }
}

struct MappedProviderFailure {
    error: ProviderError,
    account_failure: Option<CodexAccountFailure>,
    /// 原始上游错误描述，仅在凭据错误状态下持久化。
    error_message: Option<String>,
    cyber_policy_failure: bool,
    set_cookie_headers: Vec<String>,
    rate_limit_headers: Vec<(String, String)>,
    observation: Option<ProviderResponseObservation>,
    capture_response_cookies: bool,
}

impl MappedProviderFailure {
    fn plain(error: ProviderError) -> Self {
        Self {
            error,
            account_failure: None,
            error_message: None,
            cyber_policy_failure: false,
            set_cookie_headers: Vec::new(),
            rate_limit_headers: Vec::new(),
            observation: None,
            capture_response_cookies: false,
        }
    }
}

enum PreCommitPoll<T> {
    Upstream(T),
    GraceElapsed,
}

async fn wait_for_replay_grace(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
    } else {
        std::future::pending::<()>().await;
    }
}

/// 提交边界前的上游事件预取。
///
/// 原始 chunk 计数而不是重编码后的 event 大小。时间与 64 KiB 共同限定无感换号
/// 窗口；任一边界到达都会提交已缓存 wire，不能因网关私有资源规则伪造上游协议
/// 失败。一旦提交，后续事件不再具备无痕重放资格。
struct PreCommitClientEvents {
    pending: Vec<ProviderEvent>,
    prefetched_bytes: usize,
    replay_grace_deadline: Option<Instant>,
    committed: bool,
}

impl PreCommitClientEvents {
    const fn new() -> Self {
        Self {
            pending: Vec::new(),
            prefetched_bytes: 0,
            replay_grace_deadline: None,
            committed: false,
        }
    }

    fn observe_chunk(&mut self, bytes: usize) {
        if !self.committed {
            self.prefetched_bytes = self.prefetched_bytes.saturating_add(bytes);
        }
    }

    fn stage(
        &mut self,
        incoming: Vec<ProviderEvent>,
        timing_signals: ResponseEventSignals,
        completed: bool,
    ) -> Vec<ProviderEvent> {
        if self.committed {
            return incoming;
        }
        let starts_replay_grace = incoming.iter().any(ProviderEvent::has_client_event);
        self.pending.extend(incoming);
        if timing_signals.semantic_output
            || completed
            || self.prefetched_bytes > MAX_STREAM_PREFETCH_BYTES
        {
            return self.commit_pending();
        }
        if starts_replay_grace && self.replay_grace_deadline.is_none() {
            self.replay_grace_deadline = Instant::now().checked_add(STREAM_REPLAY_GRACE);
        }
        Vec::new()
    }

    fn finish(
        &mut self,
        incoming: Vec<ProviderEvent>,
        timing_signals: ResponseEventSignals,
        completed: bool,
    ) -> Vec<ProviderEvent> {
        let events = self.stage(incoming, timing_signals, completed);
        if self.committed {
            events
        } else {
            self.commit_pending()
        }
    }

    fn commit(&mut self, incoming: Vec<ProviderEvent>) -> Vec<ProviderEvent> {
        if self.committed {
            return incoming;
        }
        self.pending.extend(incoming);
        self.commit_pending()
    }

    fn take_for_failure(&mut self, incoming: Vec<ProviderEvent>) -> Vec<ProviderEvent> {
        self.pending.extend(incoming);
        self.prefetched_bytes = 0;
        self.replay_grace_deadline = None;
        std::mem::take(&mut self.pending)
    }

    const fn replay_grace_deadline(&self) -> Option<Instant> {
        self.replay_grace_deadline
    }

    const fn is_committed(&self) -> bool {
        self.committed
    }

    fn commit_pending(&mut self) -> Vec<ProviderEvent> {
        self.committed = true;
        self.prefetched_bytes = 0;
        self.replay_grace_deadline = None;
        std::mem::take(&mut self.pending)
    }
}

#[derive(Clone, Copy)]
enum ReplayBoundary {
    BeforeSemanticOutput,
    AfterSemanticOutput,
}

impl ReplayBoundary {
    const fn from_semantic_output(semantic_output_seen: bool) -> Self {
        if semantic_output_seen {
            Self::AfterSemanticOutput
        } else {
            Self::BeforeSemanticOutput
        }
    }

    const fn permits_provider_proof(self) -> bool {
        matches!(self, Self::BeforeSemanticOutput)
    }
}

struct OpenAiFailureContext<'a> {
    client: &'a CodexBackendClient,
    selector: &'a CodexCredentialSelector,
    quota: &'a Arc<CodexCredentialQuotaService>,
    response_origin: &'a Url,
    cyber_policy_scope: Option<&'a CodexCyberPolicyScope>,
    allows_account_state_mutation: bool,
}

async fn apply_failure(
    context: &OpenAiFailureContext<'_>,
    account: &ProviderAccount,
    failure: &MappedProviderFailure,
) {
    if !context.allows_account_state_mutation {
        return;
    }
    synchronize_passive_quota_headers(context.quota, account, &failure.rate_limit_headers).await;
    let needs_authoritative_quota_refresh = matches!(
        failure.account_failure,
        Some(CodexAccountFailure::QuotaExhausted | CodexAccountFailure::UsageLimitExhausted { .. })
    );
    if failure.cyber_policy_failure {
        context
            .selector
            .record_cyber_policy_failure(context.cyber_policy_scope, account)
            .await;
    }
    if let Some(account_failure) = failure.account_failure {
        context
            .client
            .evict_websocket_account(account.id().as_str())
            .await;
        if let Err(error) = context
            .selector
            .record_failure(account, account_failure, failure.error_message.clone())
            .await
        {
            tracing::warn!(
                account_id = %account.id(),
                error = %error,
                "Failed to persist OpenAI account failure state"
            );
        }
    }
    if needs_authoritative_quota_refresh {
        schedule_authoritative_quota_refresh_after_failure(context.quota, account);
    }
    if failure.capture_response_cookies
        && !failure.set_cookie_headers.is_empty()
        && let Err(error) = context
            .selector
            .capture_response_cookies(
                account,
                context.response_origin,
                &failure.set_cookie_headers,
            )
            .await
    {
        tracing::warn!(
            account_id = %account.id(),
            error = %error,
            "Failed to persist OpenAI response cookies"
        );
    }
}

fn schedule_authoritative_quota_refresh_after_failure(
    quota: &Arc<CodexCredentialQuotaService>,
    account: &ProviderAccount,
) {
    // 限额错误可以确认账号状态，但 Responses 事件中的 used_percent 可能仍停在上一结算点。
    // usage 快照在后台补齐展示基线，不得撤销同一轮真实失败，也不能阻塞原始响应。
    // 先等待 2 秒让上游结算，再查询；5 秒仅限制实际查询自身。
    let quota = Arc::clone(quota);
    let account_id = account.id().clone();
    drop(tokio::spawn(async move {
        tokio::time::sleep(QUOTA_FAILURE_REFRESH_DELAY).await;
        match tokio::time::timeout(
            QUOTA_FAILURE_REFRESH_TIMEOUT,
            quota.refresh_account_after_failure(&account_id),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(
                    account_id = %account_id,
                    error = %error,
                    "OpenAI authoritative quota synchronization after quota rejection failed"
                );
            }
            Err(_) => {
                tracing::warn!(
                    account_id = %account_id,
                    timeout_ms = QUOTA_FAILURE_REFRESH_TIMEOUT.as_millis(),
                    "OpenAI authoritative quota synchronization after quota rejection timed out"
                );
            }
        }
    }));
}

fn map_handshake_error(error: CodexClientError) -> MappedProviderFailure {
    map_client_error(error, UpstreamSendState::Ambiguous, true)
}

fn continuation_replay_required_error() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidRequest,
        UpstreamSendState::NotSent,
    )
    .with_continuation_failure(ContinuationFailure::HistoryUnavailable)
    .with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
        PREVIOUS_RESPONSE_NOT_FOUND_MESSAGE,
        Some(PREVIOUS_RESPONSE_NOT_FOUND_CODE.to_owned()),
        Some("invalid_request_error".to_owned()),
    ))
}

fn map_stream_error(error: CodexClientError) -> MappedProviderFailure {
    let allows_pre_delivery_retry = stream_transport_allows_pre_delivery_retry(&error);
    let mut failure = map_client_error(error, UpstreamSendState::Sent, false);
    if allows_pre_delivery_retry {
        failure.error = failure.error.with_pre_delivery_retry();
    }
    failure
}

fn stream_transport_allows_pre_delivery_retry(error: &CodexClientError) -> bool {
    match error {
        CodexClientError::Http(_)
        | CodexClientError::HttpJson(_)
        | CodexClientError::StreamIdleTimeout { .. } => true,
        CodexClientError::WebSocket(error) => matches!(
            error,
            CodexWebSocketExchangeError::Transport(_)
                | CodexWebSocketExchangeError::PostSendAmbiguous { .. }
                | CodexWebSocketExchangeError::SendTimeout { .. }
                | CodexWebSocketExchangeError::ClosedBeforeTerminal(_)
                | CodexWebSocketExchangeError::ReceiveIdleTimeout { .. }
                | CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstEvent { .. }
                | CodexWebSocketExchangeError::InitialEventTimeout { .. }
        ),
        _ => false,
    }
}

fn map_canonical_error(
    error: CodexCanonicalError,
    diagnostics: &CodexUpstreamDiagnostics,
    set_cookie_headers: &[String],
    rate_limit_headers: &[(String, String)],
    replay_boundary: ReplayBoundary,
) -> MappedProviderFailure {
    match error {
        CodexCanonicalError::Protocol(error) => MappedProviderFailure::plain(error),
        CodexCanonicalError::Upstream(failure) => map_upstream_failure(
            CodexUpstreamFailure::from_sse_failure(
                &failure,
                diagnostics,
                set_cookie_headers,
                rate_limit_headers,
                CodexUpstreamSendPhase::AfterPayload,
            ),
            None,
            replay_boundary,
        ),
    }
}

fn map_client_error(
    error: CodexClientError,
    uncertain_state: UpstreamSendState,
    observe_transport: bool,
) -> MappedProviderFailure {
    let continuation_failure = match &error {
        CodexClientError::WebSocket(CodexWebSocketExchangeError::ContinuationUnavailable {
            reason: PreviousResponseUnavailableReason::ConnectionBusy,
        }) => Some(ContinuationFailure::Busy),
        CodexClientError::WebSocket(CodexWebSocketExchangeError::ContinuationUnavailable {
            ..
        }) => Some(ContinuationFailure::HistoryUnavailable),
        _ => None,
    };
    let observation = observe_transport
        .then(|| codex_error_observation(&error))
        .flatten();
    if let Some(failure) = error.upstream_failure() {
        return map_upstream_failure(failure, observation, ReplayBoundary::BeforeSemanticOutput);
    }
    let mut failure = match error {
        CodexClientError::Upstream { .. } => MappedProviderFailure::plain(provider_error(
            ProviderErrorKind::Protocol,
            UpstreamSendState::Sent,
        )),
        CodexClientError::InvalidHeaderName(_)
        | CodexClientError::InvalidHeaderValue(_)
        | CodexClientError::WebSocketEncode(_)
        | CodexClientError::RequestBodyEncode(_)
        | CodexClientError::RequestCompression(_)
        | CodexClientError::ModelCatalog(_)
        | CodexClientError::CustomCa(_) => MappedProviderFailure::plain(provider_error(
            ProviderErrorKind::Protocol,
            UpstreamSendState::NotSent,
        )),
        CodexClientError::StreamIdleTimeout { .. } => MappedProviderFailure::plain(provider_error(
            ProviderErrorKind::Timeout,
            UpstreamSendState::Sent,
        )),
        CodexClientError::InvalidSse(_) => MappedProviderFailure::plain(provider_error(
            ProviderErrorKind::Protocol,
            UpstreamSendState::Sent,
        )),
        CodexClientError::Http(error) | CodexClientError::HttpJson(error) => {
            let send_state = if error.is_connect() {
                UpstreamSendState::NotSent
            } else {
                uncertain_state
            };
            MappedProviderFailure::plain(provider_error(
                if error.is_timeout() {
                    ProviderErrorKind::Timeout
                } else {
                    ProviderErrorKind::Transport
                },
                send_state,
            ))
        }
        CodexClientError::WebSocket(error) => {
            let client_visible_error = websocket_client_visible_error(&error);
            let mut failure = MappedProviderFailure::plain(provider_error(
                websocket_error_kind(&error),
                websocket_send_state(&error),
            ));
            if let Some(client_visible_error) = client_visible_error {
                failure.error = failure
                    .error
                    .with_client_visible_upstream_error(client_visible_error);
            }
            failure
        }
    };
    if let Some(continuation_failure) = continuation_failure {
        failure.error = failure
            .error
            .with_continuation_failure(continuation_failure);
    }
    failure.observation = observation;
    failure
}

fn websocket_client_visible_error(
    error: &CodexWebSocketExchangeError,
) -> Option<ClientVisibleUpstreamError> {
    let close = error.close_before_terminal()?;
    let message = close
        .reason()
        .filter(|reason| !reason.is_empty())
        .map_or_else(|| close.to_string(), str::to_owned);
    Some(ClientVisibleUpstreamError::new(
        message,
        close.code().map(|code| code.to_string()),
        Some("websocket_close_error".to_owned()),
    ))
}

fn map_upstream_failure(
    mut failure: CodexUpstreamFailure,
    observation: Option<ProviderResponseObservation>,
    replay_boundary: ReplayBoundary,
) -> MappedProviderFailure {
    let category = failure.category();
    let cyber_policy_failure = failure
        .status
        .is_some_and(|status| status.is_client_error())
        && is_cyber_policy_code(failure.code.as_deref());
    let continuation_failure = failure
        .persistable_code()
        .filter(|code| is_history_failure_code(code))
        .map(|_| ContinuationFailure::HistoryUnavailable);
    let send_state = upstream_send_state(failure.send_phase);
    let mut error = provider_error(provider_error_kind(category), send_state);
    if let Some(message) = failure.client_message.as_ref() {
        error = error.with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
            message.clone(),
            failure.client_code.clone(),
            failure.client_error_type.clone(),
        ));
    }
    if let Some(response) = failure.client_response.take() {
        let response = (*response).into_parts();
        error = error.with_client_visible_upstream_response(
            ClientVisibleUpstreamResponse::new(
                response.status,
                response.content_type,
                response.body,
            )
            .with_headers(
                response
                    .client_headers
                    .into_iter()
                    .map(|(name, value)| ProviderResponseHeader::new(name, value))
                    .collect(),
            ),
        );
    }
    if let Some(status) = failure.status {
        error = error.with_status(status.as_u16());
    }
    if replay_boundary.permits_provider_proof()
        && (failure.replay_is_safe() || cyber_policy_failure)
    {
        error = error.with_replay_safe();
    }
    if let Some(continuation_failure) = continuation_failure {
        error = error.with_continuation_failure(continuation_failure);
    }
    if let Some(retry_after) = failure.retry_after_seconds.map(Duration::from_secs) {
        error = error.with_retry_after(retry_after);
    }
    if let Some(code) = failure.persistable_code() {
        error = error.with_upstream_code(OpaqueUpstreamValue::new(code.to_owned()));
    }
    if let Some(request_id) = failure.request_id.as_deref() {
        error = error.with_upstream_request_id(OpaqueUpstreamValue::new(request_id.to_owned()));
    }
    MappedProviderFailure {
        error,
        account_failure: account_failure(
            category,
            failure.retry_after_seconds,
            failure.usage_limit_resets_at,
        ),
        error_message: failure.client_message,
        cyber_policy_failure,
        set_cookie_headers: failure.set_cookie_headers,
        rate_limit_headers: failure.rate_limit_headers,
        observation,
        capture_response_cookies: !matches!(
            category,
            CodexFailureCategory::CloudflareChallenge | CodexFailureCategory::CloudflarePathBlocked
        ),
    }
}

fn is_cyber_policy_code(code: Option<&str>) -> bool {
    code.is_some_and(|code| code.trim().eq_ignore_ascii_case("cyber_policy"))
}

fn is_history_failure_code(code: &str) -> bool {
    matches!(
        code,
        "previous_response_not_found"
            | "invalid_encrypted_content"
            | "missing_tool_output"
            | "no_tool_output"
    )
}

const fn provider_error_kind(category: CodexFailureCategory) -> ProviderErrorKind {
    match category {
        CodexFailureCategory::ModelUnsupported => ProviderErrorKind::Unsupported,
        CodexFailureCategory::CredentialExpired => ProviderErrorKind::Unauthorized,
        CodexFailureCategory::IdentityVerificationRequired | CodexFailureCategory::Banned => {
            ProviderErrorKind::PermissionDenied
        }
        CodexFailureCategory::UsageLimitExhausted => ProviderErrorKind::QuotaExhausted,
        CodexFailureCategory::RateLimited => ProviderErrorKind::RateLimited,
        CodexFailureCategory::QuotaExhausted => ProviderErrorKind::QuotaExhausted,
        CodexFailureCategory::CloudflareChallenge
        | CodexFailureCategory::CloudflarePathBlocked
        | CodexFailureCategory::Unavailable => ProviderErrorKind::Unavailable,
        CodexFailureCategory::InvalidRequest => ProviderErrorKind::InvalidRequest,
        CodexFailureCategory::PermissionDenied => ProviderErrorKind::PermissionDenied,
        CodexFailureCategory::Timeout => ProviderErrorKind::Timeout,
        CodexFailureCategory::Transport => ProviderErrorKind::Transport,
    }
}

const fn upstream_send_state(phase: CodexUpstreamSendPhase) -> UpstreamSendState {
    match phase {
        CodexUpstreamSendPhase::BeforePayload => UpstreamSendState::NotSent,
        CodexUpstreamSendPhase::AfterPayload => UpstreamSendState::Sent,
        CodexUpstreamSendPhase::Ambiguous => UpstreamSendState::Ambiguous,
    }
}

fn account_failure(
    category: CodexFailureCategory,
    retry_after_seconds: Option<u64>,
    usage_limit_resets_at: Option<i64>,
) -> Option<CodexAccountFailure> {
    match category {
        CodexFailureCategory::CredentialExpired => Some(CodexAccountFailure::CredentialExpired),
        CodexFailureCategory::IdentityVerificationRequired => {
            Some(CodexAccountFailure::IdentityVerificationRequired)
        }
        CodexFailureCategory::Banned => Some(CodexAccountFailure::Banned),
        CodexFailureCategory::UsageLimitExhausted => {
            Some(CodexAccountFailure::UsageLimitExhausted {
                reset_at: usage_limit_resets_at
                    .and_then(|seconds| u64::try_from(seconds).ok())
                    .and_then(|seconds| {
                        SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
                    }),
            })
        }
        CodexFailureCategory::RateLimited => Some(CodexAccountFailure::RateLimited {
            retry_after: retry_after_seconds.map(Duration::from_secs),
        }),
        CodexFailureCategory::QuotaExhausted => Some(CodexAccountFailure::QuotaExhausted),
        CodexFailureCategory::CloudflareChallenge => {
            Some(CodexAccountFailure::CloudflareChallenge {
                retry_after: retry_after_seconds.map(Duration::from_secs),
            })
        }
        CodexFailureCategory::CloudflarePathBlocked => {
            Some(CodexAccountFailure::CloudflarePathBlocked)
        }
        CodexFailureCategory::ModelUnsupported
        | CodexFailureCategory::InvalidRequest
        | CodexFailureCategory::PermissionDenied
        | CodexFailureCategory::Timeout
        | CodexFailureCategory::Unavailable
        | CodexFailureCategory::Transport => None,
    }
}

const fn websocket_send_state(error: &CodexWebSocketExchangeError) -> UpstreamSendState {
    match error {
        CodexWebSocketExchangeError::InvalidRequest(_)
        | CodexWebSocketExchangeError::Connect(_)
        | CodexWebSocketExchangeError::ConnectTimeout { .. }
        | CodexWebSocketExchangeError::FastPathTimeout { .. }
        | CodexWebSocketExchangeError::OriginCircuitOpen
        | CodexWebSocketExchangeError::OriginHalfOpenBusy
        | CodexWebSocketExchangeError::SharedConnectFailed
        | CodexWebSocketExchangeError::ContinuationUnavailable { .. } => UpstreamSendState::NotSent,
        CodexWebSocketExchangeError::Upstream(_)
        | CodexWebSocketExchangeError::InvalidSse(_)
        | CodexWebSocketExchangeError::UnexpectedBinaryEvent => UpstreamSendState::Sent,
        CodexWebSocketExchangeError::Transport(_)
        | CodexWebSocketExchangeError::PostSendAmbiguous { .. }
        | CodexWebSocketExchangeError::SendTimeout { .. }
        | CodexWebSocketExchangeError::ClosedBeforeTerminal(_)
        | CodexWebSocketExchangeError::ReceiveIdleTimeout { .. }
        | CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstEvent { .. }
        | CodexWebSocketExchangeError::InitialEventTimeout { .. } => UpstreamSendState::Ambiguous,
    }
}

const fn websocket_error_kind(error: &CodexWebSocketExchangeError) -> ProviderErrorKind {
    match error {
        CodexWebSocketExchangeError::InvalidRequest(_)
        | CodexWebSocketExchangeError::InvalidSse(_)
        | CodexWebSocketExchangeError::UnexpectedBinaryEvent => ProviderErrorKind::Protocol,
        CodexWebSocketExchangeError::ConnectTimeout { .. }
        | CodexWebSocketExchangeError::FastPathTimeout { .. }
        | CodexWebSocketExchangeError::SendTimeout { .. }
        | CodexWebSocketExchangeError::ReceiveIdleTimeout { .. }
        | CodexWebSocketExchangeError::InitialEventTimeout { .. } => ProviderErrorKind::Timeout,
        CodexWebSocketExchangeError::OriginCircuitOpen
        | CodexWebSocketExchangeError::OriginHalfOpenBusy
        | CodexWebSocketExchangeError::SharedConnectFailed
        | CodexWebSocketExchangeError::ContinuationUnavailable { .. } => {
            ProviderErrorKind::Unavailable
        }
        CodexWebSocketExchangeError::Upstream(_) => ProviderErrorKind::Unavailable,
        CodexWebSocketExchangeError::Transport(_)
        | CodexWebSocketExchangeError::Connect(_)
        | CodexWebSocketExchangeError::PostSendAmbiguous { .. }
        | CodexWebSocketExchangeError::ClosedBeforeTerminal(_)
        | CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstEvent { .. } => {
            ProviderErrorKind::Transport
        }
    }
}

fn provider_error(kind: ProviderErrorKind, send_state: UpstreamSendState) -> ProviderError {
    ProviderError::new(kind, send_state)
}

fn remaining(deadline: SystemTime) -> Option<Duration> {
    deadline
        .duration_since(SystemTime::now())
        .ok()
        .filter(|remaining| !remaining.is_zero())
}

const WORKER_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const WORKER_MAXIMUM_BACKOFF: Duration = Duration::from_secs(60);
const WORKER_LEASE_TTL: Duration = Duration::from_secs(15 * 60);
const WORKER_LEASE_RENEWAL: Duration = Duration::from_secs(5 * 60);
const OAUTH_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const DESKTOP_RELEASE_WORKER_OWNER: &str = "openai-desktop-release";
const MODEL_ETAG_WORKER_OWNER: &str = "openai-model-etag";

pub(crate) fn worker_contributions(
    refresh: Arc<CodexCredentialRefreshService>,
    quota: Arc<CodexCredentialQuotaService>,
    catalog: Arc<CodexCredentialCatalogService>,
    quota_refresh_policy: CodexQuotaRefreshPolicy,
    oauth_refresh_enabled: bool,
    desktop_release: Arc<CodexDesktopReleaseService>,
) -> Result<Vec<WorkerContribution>, WorkerDefinitionError> {
    let refresh_id = WorkerId::try_new(WorkerKind::OAuthRefresh, PROVIDER_NAME)?;
    let quota_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, PROVIDER_NAME)?;
    let etag_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, MODEL_ETAG_WORKER_OWNER)?;
    let desktop_release_id =
        WorkerId::try_new(WorkerKind::QuotaCatalogHealth, DESKTOP_RELEASE_WORKER_OWNER)?;
    let mut contributions = Vec::new();
    if oauth_refresh_enabled {
        contributions.push(WorkerContribution::Registration(scheduled_registration(
            refresh_id,
            OAUTH_REFRESH_INTERVAL,
            Box::new(OpenAiOAuthRefreshTask { service: refresh }),
        )?));
    }
    contributions.extend([
        WorkerContribution::Registration(scheduled_registration(
            quota_id,
            quota_refresh_policy.interval(),
            Box::new(OpenAiQuotaTask {
                quota,
                catalog: Arc::clone(&catalog),
            }),
        )?),
        WorkerContribution::Registration(WorkerRegistration::try_new(
            etag_id,
            WorkerRunnable::Daemon {
                restart: DaemonRestartPolicy::try_new(
                    WORKER_INITIAL_BACKOFF,
                    WORKER_MAXIMUM_BACKOFF,
                )?,
                task: Box::new(OpenAiCatalogEtagTask { catalog }),
            },
        )?),
        WorkerContribution::Registration(scheduled_registration(
            desktop_release_id,
            APPCAST_POLL_INTERVAL,
            Box::new(OpenAiDesktopReleaseTask {
                service: desktop_release,
            }),
        )?),
    ]);
    Ok(contributions)
}

fn scheduled_registration(
    id: WorkerId,
    interval: Duration,
    task: Box<dyn ScheduledTask>,
) -> Result<WorkerRegistration, WorkerDefinitionError> {
    let schedule = WorkerSchedule::try_new(
        interval,
        WORKER_INITIAL_BACKOFF,
        WORKER_MAXIMUM_BACKOFF,
        WORKER_LEASE_TTL,
        WORKER_LEASE_RENEWAL,
    )?;
    let lease = WorkerLeaseRequest::try_new(id.clone(), WORKER_LEASE_TTL)?;
    WorkerRegistration::try_new(
        id,
        WorkerRunnable::Scheduled {
            schedule,
            lease: Some(lease),
            task,
        },
    )
}

struct OpenAiOAuthRefreshTask {
    service: Arc<CodexCredentialRefreshService>,
}

impl ScheduledTask for OpenAiOAuthRefreshTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            if context.cancellation().is_cancelled() {
                return Ok(());
            }
            let outcomes = self.service.refresh_due().await.map_err(|error| {
                tracing::error!(error = %error, "OpenAI OAuth refresh cycle failed");
                WorkerTaskError::safe("OpenAI OAuth refresh failed")
            })?;
            let mut refreshed = 0_u64;
            let mut invalidated = 0_u64;
            let mut banned = 0_u64;
            let mut transient = 0_u64;
            let mut lease_unavailable = 0_u64;
            let mut stale = 0_u64;
            let mut failed = 0_u64;
            let mut transient_accounts = Vec::new();
            let mut failed_accounts = Vec::new();
            for outcome in &outcomes {
                match outcome {
                    CodexCredentialRefreshOutcome::Refreshed { .. } => refreshed += 1,
                    CodexCredentialRefreshOutcome::Invalidated { .. } => invalidated += 1,
                    CodexCredentialRefreshOutcome::Banned { .. } => banned += 1,
                    CodexCredentialRefreshOutcome::Transient { account_id } => {
                        transient += 1;
                        transient_accounts.push(account_id);
                    }
                    CodexCredentialRefreshOutcome::LeaseUnavailable { .. } => {
                        lease_unavailable += 1;
                    }
                    CodexCredentialRefreshOutcome::Stale { .. } => stale += 1,
                    CodexCredentialRefreshOutcome::Failed { account_id } => {
                        failed += 1;
                        failed_accounts.push(account_id);
                    }
                }
            }
            if !outcomes.is_empty() {
                tracing::info!(
                    refreshed,
                    invalidated,
                    banned,
                    transient,
                    lease_unavailable,
                    stale,
                    failed,
                    "OpenAI OAuth refresh cycle completed"
                );
            }
            if transient > 0 || failed > 0 {
                tracing::warn!(
                    refreshed,
                    invalidated,
                    banned,
                    transient,
                    lease_unavailable,
                    stale,
                    failed,
                    transient_accounts = ?transient_accounts,
                    failed_accounts = ?failed_accounts,
                    "OpenAI OAuth refresh cycle contained operational failures"
                );
            }
            Ok(())
        })
    }
}

struct OpenAiQuotaTask {
    quota: Arc<CodexCredentialQuotaService>,
    catalog: Arc<CodexCredentialCatalogService>,
}

struct OpenAiCatalogEtagTask {
    catalog: Arc<CodexCredentialCatalogService>,
}

struct OpenAiDesktopReleaseTask {
    service: Arc<CodexDesktopReleaseService>,
}

impl ScheduledTask for OpenAiDesktopReleaseTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let refresh = self.service.refresh();
            tokio::pin!(refresh);
            let result = tokio::select! {
                () = context.cancellation().cancelled() => return Ok(()),
                result = &mut refresh => result,
            };
            if let Err(error) = result {
                // 上游检查失败已经作为 Provider 观察事实保存；本周期本身正常完成，
                // 避免 Host 的短退避持续请求固定官方 appcast。
                tracing::warn!(error = %error, "OpenAI Desktop release check failed");
            }
            Ok(())
        })
    }
}

impl ScheduledTask for OpenAiQuotaTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            if context.cancellation().is_cancelled() {
                return Ok(());
            }
            let mut failures = false;
            match self.quota.synchronize().await {
                Ok(summary) if summary.has_operational_failures() => {
                    tracing::warn!(
                        updated = summary.updated,
                        exhausted = summary.exhausted,
                        banned = summary.banned,
                        transient = summary.transient,
                        stale = summary.stale,
                        "OpenAI quota cycle contained operational failures"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    failures = true;
                    tracing::warn!(
                        error = %error,
                        "OpenAI quota synchronization failed"
                    );
                }
            }
            if context.cancellation().is_cancelled() {
                return Ok(());
            }
            match self.catalog.refresh_catalogs().await {
                Ok(_) | Err(CodexCredentialCatalogError::NoEligibleCredential) => {}
                Err(error) => {
                    failures = true;
                    tracing::warn!(error = %error, "OpenAI model catalog refresh failed");
                }
            }
            if failures {
                Err(WorkerTaskError::safe(
                    "OpenAI quota or catalog synchronization failed",
                ))
            } else {
                Ok(())
            }
        })
    }
}

impl DaemonTask for OpenAiCatalogEtagTask {
    fn run(
        &self,
        cancellation: gateway_core::engine::CancellationToken,
    ) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            loop {
                tokio::select! {
                    () = cancellation.cancelled() => return Ok(()),
                    () = self.catalog.wait_for_etag_refresh() => {},
                };
                if let Err(error) = self.catalog.refresh().await {
                    tracing::warn!(
                        error = %error,
                        "OpenAI model catalog ETag refresh failed"
                    );
                }
            }
        })
    }
}
