//! Codex 的 `gateway-core` Provider adapter。

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use futures::{StreamExt, future::BoxFuture};
use gateway_core::engine::continuation::{ContinuationBinding, NativeContinuationScope};
use gateway_core::engine::credential::{AccountFeedbackStats, ProviderAccount};
use gateway_core::engine::provider::{
    EventStream, Provider, ProviderCallMetadata, ProviderCatalogGeneration,
    ProviderModelCapabilities, ProviderRequest, ProviderRequestObservation, ProviderResource,
    ProviderStream, UpstreamTransport,
};
use gateway_core::engine::{
    AttemptContext, CancellationToken, ContinuationAttempt, UpstreamSendState,
};
use gateway_core::error::{
    ClientVisibleUpstreamError, ClientVisibleUpstreamResponse, ContinuationFailure,
    OpaqueUpstreamValue, ProviderError, ProviderErrorKind,
};
use gateway_core::event::{
    GatewayEvent, ProviderEvent, ProviderResponseHeader, ProviderResponseMetadata,
    ProviderResponseObservation, ProviderResponseTimings, UpstreamHttpVersion, WebSocketPoolKind,
};
use gateway_core::operation::{GenerateRequest, Operation, OperationKind, ProviderSessionState};
use gateway_core::provider_ports::ProviderSessionAffinityKey;
use gateway_core::routing::{ModelCapabilities, ModelPresentation, ProviderKind, UpstreamModelId};
use gateway_core::task::{
    DaemonRestartPolicy, DaemonTask, ScheduledTask, WorkerContribution, WorkerCycleContext,
    WorkerDefinitionError, WorkerId, WorkerKind, WorkerLeaseRequest, WorkerRegistration,
    WorkerRunnable, WorkerSchedule, WorkerTaskError,
};
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use url::Url;

use crate::credential::{
    CodexAccountFailure, CodexAgentIdentityTaskService, CodexCredentialCatalogError,
    CodexCredentialCatalogService, CodexCredentialLease, CodexCredentialQuotaService,
    CodexCredentialRefreshOutcome, CodexCredentialRefreshService, CodexCredentialSelector,
    CodexCyberPolicyScope, CodexQuotaRefreshPolicy, CredentialSelectionError, RuntimeCodexCookie,
    SelectCodexCredential, derive_codex_cyber_policy_session_key,
    derive_codex_session_affinity_key,
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
    APPCAST_POLL_INTERVAL, CodexCliReleaseService, CodexDesktopReleaseService,
    CodexWireProfileState,
};
use crate::transport::protocol::responses::{
    CodexResponsesRequest, PreviousResponseScope, ResponseEventSignals,
};
use crate::transport::request::{
    CodexRequestEncodeError, encode_generate_request, sanitize_cross_account_item,
    scope_request_to_account,
};
use crate::transport::session::CodexSessionIdentity;
use crate::transport::websocket::{CodexWebSocketExchangeError, PreviousResponseUnavailableReason};
use crate::transport::{
    CODEX_RESPONSES_PATH, CodexAccountSelectionTelemetry, CodexBackendClient,
    CodexBackendStreamingResponse, CodexBackendTransport, CodexClientError,
    CodexRateLimitHeaderUpdates, CodexRequestContext, CodexResponseMetadata, CodexTransportMetrics,
    CodexUpstreamDiagnostics, CodexWebSocketPool, endpoint_url,
};

const PROVIDER_NAME: &str = "openai";
const HTTP_SSE_TRANSPORT: &str = "http_sse";
const WEBSOCKET_TRANSPORT: &str = "websocket";
const MAX_COOKIE_HEADER_BYTES: usize = 16 * 1024;
/// 提交边界前最多保留 64 KiB 原始上游 chunk；达到阈值后结束无感换号窗口，
/// 但不会把上游数据改写成协议失败。
const MAX_STREAM_PREFETCH_BYTES: usize = 64 * 1024;
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
    agent_identity: Arc<CodexAgentIdentityTaskService>,
    account_feedback: Arc<AccountFeedbackStats>,
    client: CodexBackendClient,
    responses_url: Url,
    session_identity: Option<CodexSessionIdentity>,
}

impl CodexProvider {
    // Provider 构造集中装配独立领域服务和透明传输依赖，拆分参数会模糊所有权。
    #[expect(clippy::too_many_arguments)]
    pub fn new(
        selector: Arc<CodexCredentialSelector>,
        catalog: Arc<CodexCredentialCatalogService>,
        quota: Arc<CodexCredentialQuotaService>,
        agent_identity: Arc<CodexAgentIdentityTaskService>,
        account_feedback: Arc<AccountFeedbackStats>,
        http: Client,
        profile: CodexWireProfileState,
        base_url: String,
        websocket_pool: Arc<CodexWebSocketPool>,
    ) -> Result<Self, CodexProviderConfigError> {
        let responses_url = Url::parse(&endpoint_url(&base_url, CODEX_RESPONSES_PATH))
            .map_err(|_| CodexProviderConfigError::InvalidBaseUrl)?;
        let client =
            CodexBackendClient::new(http, base_url, profile).with_websocket_pool(websocket_pool);
        Ok(Self {
            selector,
            catalog,
            quota,
            agent_identity,
            account_feedback,
            client,
            responses_url,
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
            .map(|mut encoded| {
                if let Some(previous) = decode_openai_session_state(request) {
                    let mut input = previous
                        .transcript
                        .iter()
                        .map(OpenAiReplayItem::value)
                        .cloned()
                        .collect::<Vec<_>>();
                    input.extend(encoded.input().iter().cloned());
                    encoded.set_input(input);
                }
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
        let Operation::Generate(generate) = request.operation() else {
            return Err(provider_error(
                ProviderErrorKind::Unsupported,
                UpstreamSendState::NotSent,
            ));
        };
        let previous_session = decode_openai_session_state(generate);
        let continuation_requested = generate.native_continuation_requested();
        let mut upstream_request =
            encode_generate_request(generate, candidate.upstream_model().as_str())
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
        if upstream_request.turn_state.is_none() {
            upstream_request.turn_state = previous_session
                .as_ref()
                .and_then(|state| state.turn_state.clone());
        }
        let session_affinity_key = derive_codex_session_affinity_key(&upstream_request);
        let cyber_policy_session_key =
            derive_codex_cyber_policy_session_key(&upstream_request, context.client_api_key_ref());
        let request_input = upstream_request.input().to_vec();
        let transport = selected_transport(&upstream_request);
        apply_transport(&mut upstream_request, transport);

        let lease = self
            .selector
            .select_with_cyber_policy(
                &SelectCodexCredential {
                    upstream_model: candidate.upstream_model().as_str(),
                    request_url: &self.responses_url,
                    attempt: &context,
                    session_affinity_key: session_affinity_key.as_ref(),
                },
                cyber_policy_session_key.as_ref(),
            )
            .await
            .map_err(map_selection_error)?;
        let lease = Arc::new(lease);
        let provider_kind = ProviderKind::new(PROVIDER_NAME)
            .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent))?;
        let cross_account = context
            .account_state_owner()
            .is_some_and(|owner| !owner.matches(&provider_kind, lease.account_id()))
            || previous_session
                .as_ref()
                .is_some_and(|state| state.account_id != lease.account_id().as_str());
        let replay_previous_response = matches!(
            context.continuation_attempt(),
            ContinuationAttempt::ReplayOwner | ContinuationAttempt::ReplayAny
        ) || previous_session.as_ref().is_some_and(|state| {
            state.continuation_scope == OpenAiContinuationScope::ReplayRequired
        });
        if replay_previous_response && let Some(state) = previous_session.as_ref() {
            let mut input = replay_input_for_account(state, lease.account_id().as_str());
            input.reserve(request_input.len());
            input.extend(request_input.iter().cloned());
            upstream_request.set_input(input);
            upstream_request.set_previous_response_id(None);
            upstream_request.previous_response_scope = None;
            upstream_request.turn_state = None;
        }
        scope_request_to_account(
            &mut upstream_request,
            lease.installation_id(),
            cross_account,
        );
        if context.continuation_attempt() == ContinuationAttempt::Native
            && !replay_previous_response
            && let Some(continuation) = context.continuation()
        {
            match continuation {
                ContinuationBinding::Pinned(continuation) => {
                    let previous_response_scope = match previous_session
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
                            return Err(provider_error(
                                ProviderErrorKind::Protocol,
                                UpstreamSendState::NotSent,
                            ));
                        }
                        None => match continuation.scope() {
                            NativeContinuationScope::Persisted => PreviousResponseScope::Persisted,
                            NativeContinuationScope::ConnectionLocal => {
                                PreviousResponseScope::ConnectionLocal
                            }
                        },
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
        let metadata = ProviderCallMetadata::new(
            provider_kind,
            candidate.upstream_model().clone(),
            ProviderResource::Account {
                id: lease.account_id().clone(),
                revision: lease.account().revision(),
            },
            UpstreamTransport::new(transport_name(transport)).map_err(|_| {
                provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
            })?,
        );
        let response_store = upstream_request.store();
        let session_capture =
            (!continuation_requested || previous_session.is_some()).then(|| OpenAiSessionCapture {
                previous: previous_session,
                request_input,
                account_id: lease.account_id().as_str().to_owned(),
                conversation_id: upstream_request.local_conversation_id.clone(),
                turn_state: upstream_request.turn_state.clone(),
                response_store,
                continuation_scope: None,
            });
        let allows_account_state_mutation = lease.allows_account_state_mutation();
        let events = cold_response_stream(ColdResponse {
            client: self.client.clone(),
            response_origin: self.responses_url.clone(),
            request: upstream_request,
            upstream_model: candidate.upstream_model().clone(),
            transport_policy: transport,
            context,
            selector: Arc::clone(&self.selector),
            quota: Arc::clone(&self.quota),
            catalog: Arc::clone(&self.catalog),
            agent_identity: Arc::clone(&self.agent_identity),
            lease: Arc::clone(&lease),
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
    agent_identity: Arc<CodexAgentIdentityTaskService>,
    lease: Arc<CodexCredentialLease>,
    session_affinity_key: Option<ProviderSessionAffinityKey>,
    session_capture: Option<OpenAiSessionCapture>,
}

#[derive(Clone, Serialize, Deserialize)]
struct OpenAiSessionState {
    account_id: String,
    conversation_id: Option<String>,
    #[serde(default)]
    turn_state: Option<String>,
    continuation_scope: OpenAiContinuationScope,
    transcript: Vec<OpenAiReplayItem>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OpenAiContinuationScope {
    Persisted,
    ConnectionLocal,
    ReplayRequired,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OpenAiReplayItem {
    ClientInput(Value),
    SanitizedOutput(Value),
    AccountOutput { account_id: String, item: Value },
}

impl OpenAiReplayItem {
    fn value(&self) -> &Value {
        match self {
            Self::ClientInput(value)
            | Self::SanitizedOutput(value)
            | Self::AccountOutput { item: value, .. } => value,
        }
    }
}

struct OpenAiSessionCapture {
    previous: Option<OpenAiSessionState>,
    request_input: Vec<Value>,
    account_id: String,
    conversation_id: Option<String>,
    turn_state: Option<String>,
    response_store: bool,
    continuation_scope: Option<OpenAiContinuationScope>,
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
    let Some((terminal_index, output)) = events.iter().enumerate().find_map(|(index, event)| {
        terminal_response_output(event).map(|output| (index, output.to_vec()))
    }) else {
        return;
    };
    let Some(capture) = capture.take() else {
        return;
    };
    let mut transcript = capture
        .previous
        .map(|state| state.transcript)
        .unwrap_or_default();
    project_transcript_to_account(&mut transcript, &capture.account_id);
    transcript.extend(
        capture
            .request_input
            .into_iter()
            .map(OpenAiReplayItem::ClientInput),
    );
    transcript.extend(
        output
            .into_iter()
            .map(|item| OpenAiReplayItem::AccountOutput {
                account_id: capture.account_id.clone(),
                item,
            }),
    );
    let Some(continuation_scope) = capture.continuation_scope else {
        return;
    };
    let Ok(update) = encode_openai_session_state(OpenAiSessionState {
        account_id: capture.account_id,
        conversation_id: capture.conversation_id,
        turn_state: capture.turn_state,
        continuation_scope,
        transcript,
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

fn project_transcript_to_account(transcript: &mut Vec<OpenAiReplayItem>, account_id: &str) {
    *transcript = transcript
        .drain(..)
        .filter_map(|item| match item {
            OpenAiReplayItem::AccountOutput {
                account_id: owner,
                item,
            } if owner != account_id => {
                sanitize_cross_account_item(item).map(OpenAiReplayItem::SanitizedOutput)
            }
            item => Some(item),
        })
        .collect();
}

fn replay_input_for_account(state: &OpenAiSessionState, account_id: &str) -> Vec<Value> {
    state
        .transcript
        .iter()
        .filter_map(|item| match item {
            OpenAiReplayItem::ClientInput(value) | OpenAiReplayItem::SanitizedOutput(value) => {
                Some(value.clone())
            }
            OpenAiReplayItem::AccountOutput {
                account_id: owner,
                item,
            } if owner == account_id => Some(without_output_id(item.clone())),
            OpenAiReplayItem::AccountOutput { item, .. } => {
                sanitize_cross_account_item(item.clone())
            }
        })
        .collect()
}

fn without_output_id(mut item: Value) -> Value {
    if let Value::Object(object) = &mut item {
        object.remove("id");
    }
    item
}

enum CodexHandshakeAttemptError {
    Client(CodexClientError),
    Cancelled,
    Timeout,
    AgentIdentityRecovery,
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
        CodexHandshakeAttemptError::AgentIdentityRecovery => MappedProviderFailure::plain(
            provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::Sent),
        ),
    }
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
        agent_identity,
        lease,
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
        let mut authorization = lease
            .authentication()
            .authorization_header(chrono::Utc::now())
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
        let stream_commit_policy =
            StreamCommitPolicy::for_continuation(context.continuation_attempt());
        let mut response = create_response_attempt(
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
        if allows_account_state_mutation
            && let Err(CodexHandshakeAttemptError::Client(error)) = &response
        {
            match agent_identity
                .recover_after_rejected_task(
                    active_account.id(),
                    lease.authentication(),
                    error,
                )
                .await
            {
                Ok(Some(recovered)) => {
                    authorization = recovered
                        .credential
                        .authentication
                        .authorization_header(chrono::Utc::now())
                        .map_err(|_| {
                            provider_error(
                                ProviderErrorKind::Unauthorized,
                                UpstreamSendState::NotSent,
                            )
                        })?;
                    active_account = recovered.account;
                    response = create_response_attempt(
                        &client,
                        &request,
                        codex_request_context(
                            &request,
                            &request_id,
                            &active_account,
                            &recovered.credential.installation_id,
                            &authorization,
                            cookie_header.as_ref(),
                            account_selection,
                        ),
                        active_account.id().as_str(),
                        context.deadline(),
                        &cancellation,
                    )
                    .await;
                }
                Ok(None) => {}
                Err(_) => response = Err(CodexHandshakeAttemptError::AgentIdentityRecovery),
            }
        }
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
        if allows_account_state_mutation {
            synchronize_passive_quota(&quota, &active_account, &response.rate_limit_headers).await;
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
        let rate_limit_updates = response.rate_limit_header_updates;
        let turn_state_updates = response.turn_state_update;
        // OpenAI 线路为透明代理：HTTP SSE 与 WebSocket 两条上游均启用 raw 透传，
        // 下游按字节转发上游原文，避免 serde 往返改写数值/精度（大整数→f64、logprobs 等）。
        // WS 帧由 reducer 以 encode_sse_event(&event, raw) 逐字节内嵌上游原始 JSON
        // （transport/protocol/websocket.rs），push_frames 抽出的 data 即上游原文。
        let mut decoder =
            CodexCanonicalDecoder::new(upstream_model.as_str()).with_raw_sse_passthrough();
        let mut pre_commit_events = PreCommitClientEvents::new(stream_commit_policy);
        loop {
            let Some(stream_deadline) = remaining(context.deadline()) else {
                Err(provider_error(ProviderErrorKind::Timeout, UpstreamSendState::Sent))?;
                return;
            };
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
                chunk = body.next() => match chunk {
                    Some(Ok(chunk)) => Ok(Some(chunk)),
                    Some(Err(error)) => Err(map_stream_error(error)),
                    None => Ok(None),
                },
            };
            let next = match next {
                Ok(next) => next,
                Err(failure) => {
                    let updates = take_rate_limit_updates(rate_limit_updates.as_ref()).await;
                    if !updates.is_empty() {
                        if allows_account_state_mutation {
                            synchronize_passive_quota(&quota, &active_account, &updates).await;
                        }
                        if observation_state.merge_rate_limit_headers(&updates)
                            && let Some(observation) = observation_state.observation()
                        {
                            yield ProviderEvent::observation(observation);
                        }
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
                if allows_account_state_mutation {
                    synchronize_passive_quota(&quota, &active_account, &updates).await;
                }
                observation_state.merge_rate_limit_headers(&updates)
            };
            if let (Some(capture), Some(updates)) =
                (session_capture.as_mut(), turn_state_updates.as_ref())
                // take：仅在上游写入过新 turn_state 时更新 capture，避免逐 chunk 克隆。
                && let Some(turn_state) = updates.lock().await.take()
            {
                capture.turn_state = Some(turn_state);
            }
            let first_event_changed =
                observation_state.observe_stream_chunk(&chunk, context.timing_started_at());
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
                .observe_service_tier(decoder.response_service_tier());
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
                    .observe_timing_signals(timing_signals, context.timing_started_at());
            let completed = events
                .iter()
                .flat_map(ProviderEvent::canonical_facts)
                .any(|event| matches!(event, GatewayEvent::Completed(_)));
            let terminal_changed = completed
                && observation_state.mark_completed(terminal_response_is_incomplete(&events));
            if let Some((failure, _)) = terminal_failure.as_ref() {
                apply_failure(&failure_context, &active_account, failure)
                .await;
            }
            attach_openai_session_update(&mut events, &mut session_capture);
            if allows_account_state_mutation && completed && terminal_failure.is_none() {
                // 完成事件一旦交给下游，Core 可以立刻停止轮询 Provider stream；
                // 在此之前持久化亲和关系，保证成功请求不会因流被提前 drop 而丢失绑定。
                selector
                    .record_success(&active_account, session_affinity_key.as_ref())
                    .await;
                selector
                    .observe_cyber_policy_success(cyber_policy_scope.as_ref())
                    .await;
            }
            if (rate_limits_changed
                || service_tier_changed
                || timing_changed
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
        let service_tier_changed =
            observation_state.observe_service_tier(decoder.response_service_tier());
        let timing_changed = observation_state
            .observe_timing_signals(timing_signals, context.timing_started_at());
        let updates = take_rate_limit_updates(rate_limit_updates.as_ref()).await;
        let rate_limits_changed = if updates.is_empty() {
            false
        } else {
            if allows_account_state_mutation {
                synchronize_passive_quota(&quota, &active_account, &updates).await;
            }
            observation_state.merge_rate_limit_headers(&updates)
        };
        if let Some((failure, _)) = terminal_failure.as_ref() {
            apply_failure(&failure_context, &active_account, failure)
            .await;
        }
        if let (Some(capture), Some(updates)) =
            (session_capture.as_mut(), turn_state_updates.as_ref())
            && let Some(turn_state) = updates.lock().await.take()
        {
            capture.turn_state = Some(turn_state);
        }
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
                .record_success(&active_account, session_affinity_key.as_ref())
                .await;
            selector
                .observe_cyber_policy_success(cyber_policy_scope.as_ref())
                .await;
        }
        if (service_tier_changed || timing_changed || rate_limits_changed || terminal_changed)
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
    service_tier: Option<String>,
    rate_limit_headers: Vec<(String, String)>,
    account_id: String,
    attempt_index: u32,
    timings: ProviderResponseTimings,
    terminal: Option<OpenAiResponseTerminal>,
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
            // 请求档位保留在 requestSummary；一等观测只接受响应确认的实际档位。
            service_tier: None,
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
        if let Some(service_tier) = self.service_tier.as_deref() {
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
        if signals.semantic_output {
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

    fn observe_service_tier(&mut self, service_tier: Option<&str>) -> bool {
        let Some(service_tier) = service_tier else {
            return false;
        };
        if self.service_tier.as_deref() == Some(service_tier) {
            return false;
        }
        self.service_tier = Some(service_tier.to_owned());
        true
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
        if let Some(service_tier) = &self.service_tier {
            metadata.insert(
                "serviceTier".to_owned(),
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

async fn take_rate_limit_updates(
    updates: Option<&CodexRateLimitHeaderUpdates>,
) -> Vec<(String, String)> {
    let Some(updates) = updates else {
        return Vec::new();
    };
    std::mem::take(&mut *updates.lock().await)
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
    headers: &[(String, String)],
) {
    if headers.is_empty() {
        return;
    }
    if let Err(error) = quota.synchronize_passive_headers(account, headers).await {
        tracing::warn!(
            account_id = %account.id(),
            error = %error,
            "OpenAI passive quota synchronization failed"
        );
    }
}

const fn actual_transport_name(transport: CodexBackendTransport) -> &'static str {
    match transport {
        CodexBackendTransport::HttpSse => HTTP_SSE_TRANSPORT,
        CodexBackendTransport::WebSocket => WEBSOCKET_TRANSPORT,
    }
}

fn nonnegative_millis(value: Option<i64>) -> Option<u64> {
    value.and_then(|value| u64::try_from(value).ok())
}

fn compile_model_capabilities(model: &CodexCatalogModel) -> ProviderModelCapabilities {
    let evidence = model.capabilities();
    let mut operations = BTreeSet::new();
    if evidence.responses_api() == CodexCatalogCapabilityEvidence::DeclaredNative {
        operations.insert(OperationKind::Generate);
    }
    let capabilities = ModelCapabilities::new(operations, None).with_upstream_feature_validation();
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
            let error = provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent);
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
        | CredentialSelectionError::CookiePolicy => {
            provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent)
        }
    }
}

struct MappedProviderFailure {
    error: ProviderError,
    account_failure: Option<CodexAccountFailure>,
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
            cyber_policy_failure: false,
            set_cookie_headers: Vec::new(),
            rate_limit_headers: Vec::new(),
            observation: None,
            capture_response_cookies: false,
        }
    }
}

/// 与 2.x 一致的流提交策略：普通请求优先首事件，续写/重放优先可恢复性。
#[derive(Clone, Copy)]
enum StreamCommitPolicy {
    FirstForwardableEvent,
    UntilOutputOrTerminal,
}

impl StreamCommitPolicy {
    const fn for_continuation(attempt: ContinuationAttempt) -> Self {
        match attempt {
            ContinuationAttempt::None => Self::FirstForwardableEvent,
            ContinuationAttempt::Native
            | ContinuationAttempt::ReplayOwner
            | ContinuationAttempt::ReplayAny => Self::UntilOutputOrTerminal,
        }
    }

    fn boundary_reached(
        self,
        events: &[ProviderEvent],
        timing_signals: ResponseEventSignals,
        completed: bool,
    ) -> bool {
        match self {
            Self::FirstForwardableEvent => events.iter().any(ProviderEvent::has_client_event),
            Self::UntilOutputOrTerminal => timing_signals.semantic_output || completed,
        }
    }
}

/// 提交边界前的上游事件预取。
///
/// 原始 chunk 计数而不是重编码后的 event 大小。64 KiB 只限定无感换号窗口；达到
/// 阈值会提交已缓存 wire，绝不能因为网关私有资源规则伪造上游协议失败。一旦提交，
/// 后续事件不再具备无痕重放资格。
struct PreCommitClientEvents {
    policy: StreamCommitPolicy,
    pending: Vec<ProviderEvent>,
    prefetched_bytes: usize,
    committed: bool,
}

impl PreCommitClientEvents {
    const fn new(policy: StreamCommitPolicy) -> Self {
        Self {
            policy,
            pending: Vec::new(),
            prefetched_bytes: 0,
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
        let boundary_reached = self
            .policy
            .boundary_reached(&incoming, timing_signals, completed);
        self.pending.extend(incoming);
        if boundary_reached || self.prefetched_bytes > MAX_STREAM_PREFETCH_BYTES {
            return self.commit_pending();
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
        std::mem::take(&mut self.pending)
    }

    const fn is_committed(&self) -> bool {
        self.committed
    }

    fn commit_pending(&mut self) -> Vec<ProviderEvent> {
        self.committed = true;
        self.prefetched_bytes = 0;
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
    quota: &'a CodexCredentialQuotaService,
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
    synchronize_passive_quota(context.quota, account, &failure.rate_limit_headers).await;
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
            .record_failure(account, account_failure)
            .await
        {
            tracing::warn!(
                account_id = %account.id(),
                error = %error,
                "Failed to persist OpenAI account failure state"
            );
        }
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

fn map_handshake_error(error: CodexClientError) -> MappedProviderFailure {
    map_client_error(error, UpstreamSendState::Ambiguous, true)
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
        CodexClientError::Http(_) | CodexClientError::StreamIdleTimeout { .. } => true,
        CodexClientError::WebSocket(error) => matches!(
            error,
            CodexWebSocketExchangeError::Transport(_)
                | CodexWebSocketExchangeError::PostSendAmbiguous { .. }
                | CodexWebSocketExchangeError::SendTimeout { .. }
                | CodexWebSocketExchangeError::ClosedBeforeTerminal
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
        CodexClientError::Http(error) => {
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
        CodexClientError::WebSocket(error) => MappedProviderFailure::plain(provider_error(
            websocket_error_kind(&error),
            websocket_send_state(&error),
        )),
    };
    if let Some(continuation_failure) = continuation_failure {
        failure.error = failure
            .error
            .with_continuation_failure(continuation_failure);
    }
    failure.observation = observation;
    failure
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
        && (failure.replay_is_safe() || continuation_failure.is_some() || cyber_policy_failure)
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
        account_failure: account_failure(category, failure.retry_after_seconds),
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
) -> Option<CodexAccountFailure> {
    match category {
        CodexFailureCategory::CredentialExpired => Some(CodexAccountFailure::CredentialExpired),
        CodexFailureCategory::IdentityVerificationRequired => {
            Some(CodexAccountFailure::IdentityVerificationRequired)
        }
        CodexFailureCategory::Banned => Some(CodexAccountFailure::Banned),
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
        | CodexWebSocketExchangeError::ClosedBeforeTerminal
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
        | CodexWebSocketExchangeError::ClosedBeforeTerminal
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
const CLI_RELEASE_WORKER_OWNER: &str = "openai-cli-release";
const MODEL_ETAG_WORKER_OWNER: &str = "openai-model-etag";

pub(crate) fn worker_contributions(
    refresh: Arc<CodexCredentialRefreshService>,
    quota: Arc<CodexCredentialQuotaService>,
    catalog: Arc<CodexCredentialCatalogService>,
    quota_refresh_policy: CodexQuotaRefreshPolicy,
    oauth_refresh_enabled: bool,
    cli_release: Arc<CodexCliReleaseService>,
    desktop_release: Arc<CodexDesktopReleaseService>,
) -> Result<Vec<WorkerContribution>, WorkerDefinitionError> {
    let refresh_id = WorkerId::try_new(WorkerKind::OAuthRefresh, PROVIDER_NAME)?;
    let quota_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, PROVIDER_NAME)?;
    let etag_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, MODEL_ETAG_WORKER_OWNER)?;
    let desktop_release_id =
        WorkerId::try_new(WorkerKind::QuotaCatalogHealth, DESKTOP_RELEASE_WORKER_OWNER)?;
    let cli_release_id =
        WorkerId::try_new(WorkerKind::QuotaCatalogHealth, CLI_RELEASE_WORKER_OWNER)?;
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
            cli_release_id,
            APPCAST_POLL_INTERVAL,
            Box::new(OpenAiCliReleaseTask {
                service: cli_release,
            }),
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
            let outcomes = self
                .service
                .refresh_due()
                .await
                .map_err(|_| WorkerTaskError::safe("OpenAI OAuth refresh failed"))?;
            let operational_failures = outcomes
                .iter()
                .filter(|outcome| {
                    matches!(
                        outcome,
                        CodexCredentialRefreshOutcome::Transient { .. }
                            | CodexCredentialRefreshOutcome::Failed { .. }
                    )
                })
                .count();
            if operational_failures > 0 {
                tracing::warn!(
                    operational_failures,
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

struct OpenAiCliReleaseTask {
    service: Arc<CodexCliReleaseService>,
}

impl ScheduledTask for OpenAiCliReleaseTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let refresh = self.service.refresh();
            tokio::pin!(refresh);
            let result = tokio::select! {
                () = context.cancellation().cancelled() => return Ok(()),
                result = &mut refresh => result,
            };
            if let Err(error) = result {
                tracing::warn!(error = %error, "OpenAI CLI release check failed");
            }
            Ok(())
        })
    }
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
                    tracing::warn!("OpenAI quota cycle contained operational failures");
                }
                Ok(_) => {}
                Err(_) => {
                    failures = true;
                    tracing::warn!("OpenAI quota synchronization failed");
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
