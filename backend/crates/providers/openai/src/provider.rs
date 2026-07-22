//! Codex 的 `gateway-core` Provider adapter。

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures::{StreamExt, future::BoxFuture};
use gateway_core::engine::continuation::{ContinuationBinding, NativeContinuationScope};
use gateway_core::engine::provider::{
    EventStream, Provider, ProviderCallMetadata, ProviderCatalogGeneration,
    ProviderModelCapabilities, ProviderRequest, ProviderRequestObservation, ProviderResource,
    ProviderStream, UpstreamTransport,
};
use gateway_core::engine::{AttemptContext, ContinuationAttempt, UpstreamSendState};
use gateway_core::error::{
    ContinuationFailure, ProviderError, ProviderErrorKind, SafeUpstreamValue,
};
use gateway_core::event::{
    GatewayEvent, ProviderEvent, ProviderResponseHeader, ProviderResponseObservation,
    ProviderResponseTimings, UpstreamHttpVersion, WebSocketPoolKind,
};
use gateway_core::operation::{GenerateRequest, Operation, OperationKind, ProviderSessionState};
use gateway_core::routing::{ModelCapabilities, ProviderKind, UpstreamModelId};
use gateway_core::task::{
    DaemonRestartPolicy, DaemonTask, ScheduledTask, WorkerContribution, WorkerCycleContext,
    WorkerDefinitionError, WorkerId, WorkerKind, WorkerLeaseRequest, WorkerRegistration,
    WorkerRunnable, WorkerSchedule, WorkerTaskError,
};
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::credential::{
    CodexAccountFailure, CodexCredentialCatalogService, CodexCredentialLease,
    CodexCredentialQuotaService, CodexCredentialRefreshOutcome, CodexCredentialRefreshService,
    CodexCredentialSelector, CredentialSelectionError, RuntimeCodexCookie, SelectCodexCredential,
};
use crate::transport::canonical::{
    CodexCanonicalDecoder, CodexCanonicalError, CodexCanonicalOutcome,
};
use crate::transport::catalog::{CodexCatalogCapabilityEvidence, CodexCatalogModel};
use crate::transport::diagnostics::{
    CodexFailureCategory, CodexUpstreamFailure, CodexUpstreamSendPhase,
};
use crate::transport::profile::{
    APPCAST_POLL_INTERVAL, CodexCliReleaseService, CodexDesktopReleaseService,
    CodexWireProfileState,
};
use crate::transport::protocol::responses::{CodexResponsesRequest, PreviousResponseScope};
use crate::transport::request::{
    CodexRequestEncodeError, encode_generate_request, sanitize_cross_account_item,
    scope_request_to_account,
};
use crate::transport::websocket::{CodexWebSocketExchangeError, PreviousResponseUnavailableReason};
use crate::transport::{
    CODEX_RESPONSES_PATH, CodexBackendClient, CodexBackendTransport, CodexClientError,
    CodexRequestContext, CodexResponseMetadata, CodexTransportMetrics, CodexUpstreamDiagnostics,
    CodexWebSocketPool, endpoint_url,
};

const PROVIDER_NAME: &str = "openai";
const HTTP_SSE_TRANSPORT: &str = "http_sse";
const WEBSOCKET_TRANSPORT: &str = "websocket";
const MAX_COOKIE_HEADER_BYTES: usize = 16 * 1024;
pub const OFFICIAL_CODEX_BASE_PATH: &str = "/backend-api";
pub const OFFICIAL_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexProviderTransport {
    HttpOnly,
    PreferWebSocket,
}

impl CodexProviderTransport {
    fn parse_explicit(value: &str) -> Option<Self> {
        match value {
            HTTP_SSE_TRANSPORT => Some(Self::HttpOnly),
            WEBSOCKET_TRANSPORT => Some(Self::PreferWebSocket),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodexProviderConfigError {
    #[error("official Codex provider URL is invalid")]
    InvalidBaseUrl,
}

pub struct CodexProvider {
    selector: Arc<CodexCredentialSelector>,
    catalog: Arc<CodexCredentialCatalogService>,
    quota: Arc<CodexCredentialQuotaService>,
    client: CodexBackendClient,
    responses_url: Url,
}

impl CodexProvider {
    pub fn new(
        selector: Arc<CodexCredentialSelector>,
        catalog: Arc<CodexCredentialCatalogService>,
        quota: Arc<CodexCredentialQuotaService>,
        http: Client,
        profile: CodexWireProfileState,
        websocket_pool: Arc<CodexWebSocketPool>,
    ) -> Result<Self, CodexProviderConfigError> {
        let responses_url =
            Url::parse(&endpoint_url(OFFICIAL_CODEX_BASE_URL, CODEX_RESPONSES_PATH))
                .map_err(|_| CodexProviderConfigError::InvalidBaseUrl)?;
        let client = CodexBackendClient::new(http, OFFICIAL_CODEX_BASE_URL, profile)
            .with_websocket_pool(websocket_pool);
        Ok(Self {
            selector,
            catalog,
            quota,
            client,
            responses_url,
        })
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
        let semantics = encode_generate_request(request, "observability")
            .map(|mut encoded| {
                if let Ok(Some(previous)) = decode_openai_session_state(request) {
                    let mut input = previous
                        .transcript
                        .iter()
                        .map(OpenAiReplayItem::value)
                        .cloned()
                        .collect::<Vec<_>>();
                    input.extend(encoded.input().iter().cloned());
                    encoded.set_input(input);
                }
                encoded.semantics()
            })
            .unwrap_or_default();
        ProviderRequestObservation {
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
        let previous_session = decode_openai_session_state(generate)?;
        let continuation_requested = generate.continuation().is_some();
        let mut upstream_request =
            encode_generate_request(generate, candidate.upstream_model().as_str())
                .map_err(map_request_error)?;
        let request_input = upstream_request.input().to_vec();
        let transport = selected_transport(&request)?;
        apply_transport(&mut upstream_request, transport);

        let lease = self
            .selector
            .select(&SelectCodexCredential {
                upstream_model: candidate.upstream_model().as_str(),
                request_url: &self.responses_url,
                attempt: &context,
            })
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
        if replay_previous_response {
            let state = previous_session.as_ref().ok_or_else(|| {
                provider_error(
                    ProviderErrorKind::InvalidRequest,
                    UpstreamSendState::NotSent,
                )
            })?;
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
        if let Some(conversation_id) = previous_session
            .as_ref()
            .and_then(|state| state.conversation_id.as_ref())
        {
            upstream_request.local_conversation_id = Some(conversation_id.clone());
        }
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
                response_store,
                continuation_scope: None,
            });
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
            lease: Arc::clone(&lease),
            session_capture,
        });
        Ok(ProviderStream::new(metadata, events, lease))
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
    session_capture: Option<OpenAiSessionCapture>,
}

#[derive(Clone, Serialize, Deserialize)]
struct OpenAiSessionState {
    account_id: String,
    conversation_id: Option<String>,
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
    response_store: bool,
    continuation_scope: Option<OpenAiContinuationScope>,
}

fn decode_openai_session_state(
    request: &GenerateRequest,
) -> Result<Option<OpenAiSessionState>, ProviderError> {
    request
        .provider_session_state(PROVIDER_NAME)
        .map(|state| {
            serde_json::from_value(Value::Object(state.payload().clone())).map_err(|_| {
                provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
            })
        })
        .transpose()
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
) -> Result<(), ProviderError> {
    let Some((terminal_index, output)) = events.iter().enumerate().find_map(|(index, event)| {
        terminal_response_output(event).map(|output| (index, output.to_vec()))
    }) else {
        return Ok(());
    };
    let Some(capture) = capture.take() else {
        return Ok(());
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
    let update = encode_openai_session_state(OpenAiSessionState {
        account_id: capture.account_id,
        conversation_id: capture.conversation_id,
        continuation_scope: capture
            .continuation_scope
            .ok_or_else(|| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent))?,
        transcript,
    })?;
    events[terminal_index].attach_session_update(update);
    Ok(())
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
        mut session_capture,
    } = response;
    Box::pin(async_stream::try_stream! {
        let cookie_header = build_cookie_header(lease.cookies())?;
        let request_id = context.request_id().as_str().to_owned();
        let cancellation = context.cancellation().clone();
        let Some(handshake_deadline) = remaining(context.deadline()) else {
            Err(provider_error(ProviderErrorKind::Timeout, UpstreamSendState::NotSent))?;
            return;
        };
        let request_context = codex_request_context(
            &request,
            &request_id,
            &lease,
            cookie_header.as_ref(),
        );
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(MappedProviderFailure::plain(provider_error(
                ProviderErrorKind::Cancelled,
                UpstreamSendState::Ambiguous,
            ))),
            _ = tokio::time::sleep(handshake_deadline) => Err(MappedProviderFailure::plain(provider_error(
                ProviderErrorKind::Timeout,
                UpstreamSendState::Ambiguous,
            ))),
            response = client.create_response_stream_with_pool_account(
                &request,
                request_context,
                Some(lease.account_id().as_str()),
            ) => response.map_err(map_handshake_error),
        };
        let response = match response {
            Ok(response) => response,
            Err(mut failure) => {
                if let Some(observation) = failure.observation.take() {
                    yield ProviderEvent::observation(observation);
                }
                apply_failure(
                    &client,
                    &selector,
                    &quota,
                    &lease,
                    &response_origin,
                    &failure,
                )
                .await;
                Err(failure.error)?;
                return;
            }
        };
        if !accepts_backend_transport(transport_policy, response.transport) {
            Err(provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent))?;
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
        }
        yield ProviderEvent::observation(codex_response_observation(
            response.transport,
            &response.diagnostics,
            &response.response_metadata,
            &response.transport_metrics,
            response.websocket_pool_decision,
        )?);
        synchronize_passive_quota(&quota, &lease, &response.rate_limit_headers).await;
        if let Some(etag) = response.response_metadata.models_etag.as_deref()
            && let Err(error) = catalog.observe_response_etag(etag)
        {
            tracing::warn!(
                error = %error,
                "OpenAI model ETag observation was rejected"
            );
        }
        if !response.set_cookie_headers.is_empty() {
            let _ = selector
                .capture_response_cookies(&lease, &response_origin, &response.set_cookie_headers)
                .await;
        }
        let mut body = response.body;
        let failure_diagnostics = response.diagnostics.clone();
        let failure_set_cookie_headers = response.set_cookie_headers.clone();
        let failure_rate_limit_headers = response.rate_limit_headers.clone();
        let rate_limit_updates = response.rate_limit_header_updates;
        let mut decoder = CodexCanonicalDecoder::new(upstream_model.as_str());
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
                    apply_failure(
                        &client,
                        &selector,
                        &quota,
                        &lease,
                        &response_origin,
                        &failure,
                    )
                    .await;
                    Err(failure.error)?;
                    return;
                }
            };
            let Some(chunk) = next else { break; };
            if let Some(updates) = rate_limit_updates.as_ref() {
                let updates = std::mem::take(&mut *updates.lock().await);
                synchronize_passive_quota(&quota, &lease, &updates).await;
            }
            let (mut events, terminal_error) = match decoder.push(&chunk) {
                CodexCanonicalOutcome::Events(events) => (events, None),
                CodexCanonicalOutcome::Failed(failure) => {
                    let (events, error, semantic_output_seen) = failure.into_parts();
                    let failure = map_canonical_error(
                        error,
                        &failure_diagnostics,
                        &failure_set_cookie_headers,
                        &failure_rate_limit_headers,
                        ReplayBoundary::from_semantic_output(semantic_output_seen),
                    );
                    apply_failure(
                        &client,
                        &selector,
                        &quota,
                        &lease,
                        &response_origin,
                        &failure,
                    )
                    .await;
                    (events, Some(failure.error))
                }
            };
            attach_openai_session_update(&mut events, &mut session_capture)?;
            let completed = events
                .iter()
                .flat_map(ProviderEvent::canonical_facts)
                .any(|event| matches!(event, GatewayEvent::Completed(_)));
            for event in events {
                yield event;
            }
            if let Some(error) = terminal_error {
                Err(error)?;
                return;
            }
            if completed {
                selector.record_success(&lease);
                return;
            }
        }
        let (mut events, terminal_error) = match decoder.finish() {
            CodexCanonicalOutcome::Events(events) => (events, None),
            CodexCanonicalOutcome::Failed(failure) => {
                let (events, error, semantic_output_seen) = failure.into_parts();
                let failure = map_canonical_error(
                    error,
                    &failure_diagnostics,
                    &failure_set_cookie_headers,
                    &failure_rate_limit_headers,
                    ReplayBoundary::from_semantic_output(semantic_output_seen),
                );
                apply_failure(
                    &client,
                    &selector,
                    &quota,
                    &lease,
                    &response_origin,
                    &failure,
                )
                .await;
                (events, Some(failure.error))
            }
        };
        attach_openai_session_update(&mut events, &mut session_capture)?;
        let completed = events
            .iter()
            .flat_map(ProviderEvent::canonical_facts)
            .any(|event| matches!(event, GatewayEvent::Completed(_)));
        for event in events {
            yield event;
        }
        if let Some(error) = terminal_error {
            Err(error)?;
            return;
        }
        if completed {
            selector.record_success(&lease);
        }
    })
}

fn codex_response_observation(
    transport: CodexBackendTransport,
    diagnostics: &CodexUpstreamDiagnostics,
    response_metadata: &CodexResponseMetadata,
    metrics: &CodexTransportMetrics,
    websocket_pool_decision: Option<crate::transport::WebSocketPoolDecision>,
) -> Result<ProviderResponseObservation, ProviderError> {
    let mut observation = ProviderResponseObservation::new(
        UpstreamTransport::new(actual_transport_name(transport))
            .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent))?,
    )
    .with_timings(ProviderResponseTimings {
        transport_decision_wait_ms: nonnegative_millis(metrics.transport_decision_wait_ms),
        connect_ms: nonnegative_millis(metrics.ws_connect_ms),
        headers_ms: nonnegative_millis(metrics.upstream_headers_ms),
        first_event_ms: nonnegative_millis(metrics.first_event_ms),
    });
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
    if let Some(request_id) = diagnostics
        .request_id
        .as_deref()
        .and_then(|request_id| SafeUpstreamValue::new(request_id.to_owned()).ok())
    {
        observation = observation.with_request_id(request_id);
    }
    let client_headers = response_metadata
        .client_headers
        .iter()
        .filter_map(|(name, value)| {
            let value = SafeUpstreamValue::new(value.to_owned()).ok()?;
            ProviderResponseHeader::new(name.to_owned(), value)
        })
        .collect();
    observation = observation.with_client_headers(client_headers);
    Ok(observation)
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
            )
            .ok()?
            .with_status_code(status.as_u16());
        }
        CodexClientError::WebSocket(CodexWebSocketExchangeError::Upstream(upstream)) => {
            observation = observation.with_status_code(upstream.status_code);
            if let Some(request_id) = upstream
                .diagnostics
                .request_id
                .as_deref()
                .and_then(|value| SafeUpstreamValue::new(value.to_owned()).ok())
            {
                observation = observation.with_request_id(request_id);
            }
        }
        _ => {}
    }
    Some(observation)
}

async fn synchronize_passive_quota(
    quota: &CodexCredentialQuotaService,
    lease: &CodexCredentialLease,
    headers: &[(String, String)],
) {
    if headers.is_empty() {
        return;
    }
    if let Err(error) = quota
        .synchronize_passive_headers(lease.account(), headers)
        .await
    {
        tracing::warn!(
            account_id = %lease.account_id(),
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
    let context_window = model
        .limits()
        .context_window_tokens()
        .or_else(|| model.limits().max_context_window_tokens())
        .map_or(0, std::num::NonZeroU64::get);
    let capabilities =
        ModelCapabilities::new(operations, context_window, None).with_upstream_feature_validation();
    ProviderModelCapabilities::new(model.request_model().clone(), capabilities)
}

fn selected_transport(request: &ProviderRequest) -> Result<CodexProviderTransport, ProviderError> {
    let mut transport = CodexProviderTransport::PreferWebSocket;
    if let Some(value) = request
        .operation()
        .provider_options(PROVIDER_NAME)
        .and_then(|options| options.get("transport"))
    {
        transport = value
            .as_str()
            .and_then(CodexProviderTransport::parse_explicit)
            .ok_or_else(|| {
                provider_error(
                    ProviderErrorKind::InvalidRequest,
                    UpstreamSendState::NotSent,
                )
            })?;
    }
    Ok(transport)
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
    lease: &'a CodexCredentialLease,
    cookie_header: Option<&'a SecretString>,
) -> CodexRequestContext<'a> {
    CodexRequestContext {
        access_token: lease.secret().access_token.expose_secret(),
        account_id: lease.account().upstream_account_id(),
        request_id,
        turn_state: request.turn_state.as_deref(),
        turn_metadata: request.turn_metadata.as_deref(),
        beta_features: request.beta_features.as_deref(),
        include_timing_metrics: request.include_timing_metrics.as_deref(),
        version: request.version.as_deref(),
        codex_window_id: request.codex_window_id.as_deref(),
        parent_thread_id: request.parent_thread_id.as_deref(),
        cookie_header: cookie_header.map(ExposeSecret::expose_secret),
        installation_id: Some(lease.installation_id()),
        session_id: request.client_session_id.as_deref(),
        thread_id: request.client_thread_id.as_deref(),
        client_request_id: request.client_request_id.as_deref(),
        turn_id: request.client_turn_id.as_deref(),
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
        CodexRequestEncodeError::InvalidProviderOptions => ProviderErrorKind::InvalidRequest,
        CodexRequestEncodeError::UnsupportedProviderOption
        | CodexRequestEncodeError::UnsupportedContent => ProviderErrorKind::Unsupported,
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
        CredentialSelectionError::NoEligibleCredential
        | CredentialSelectionError::InvalidCredential
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
            set_cookie_headers: Vec::new(),
            rate_limit_headers: Vec::new(),
            observation: None,
            capture_response_cookies: false,
        }
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

async fn apply_failure(
    client: &CodexBackendClient,
    selector: &CodexCredentialSelector,
    quota: &CodexCredentialQuotaService,
    lease: &CodexCredentialLease,
    response_origin: &Url,
    failure: &MappedProviderFailure,
) {
    synchronize_passive_quota(quota, lease, &failure.rate_limit_headers).await;
    if let Some(account_failure) = failure.account_failure {
        client
            .evict_websocket_account(lease.account_id().as_str())
            .await;
        if let Err(error) = selector.record_failure(lease, account_failure).await {
            tracing::warn!(
                account_id = %lease.account_id(),
                error = %error,
                "Failed to persist OpenAI account failure state"
            );
        }
    }
    if failure.capture_response_cookies
        && !failure.set_cookie_headers.is_empty()
        && let Err(error) = selector
            .capture_response_cookies(lease, response_origin, &failure.set_cookie_headers)
            .await
    {
        tracing::warn!(
            account_id = %lease.account_id(),
            error = %error,
            "Failed to persist OpenAI response cookies"
        );
    }
}

fn map_handshake_error(error: CodexClientError) -> MappedProviderFailure {
    map_client_error(error, UpstreamSendState::Ambiguous, true)
}

fn map_stream_error(error: CodexClientError) -> MappedProviderFailure {
    map_client_error(error, UpstreamSendState::Sent, false)
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
    failure: CodexUpstreamFailure,
    observation: Option<ProviderResponseObservation>,
    replay_boundary: ReplayBoundary,
) -> MappedProviderFailure {
    let category = failure.category();
    let continuation_failure = failure
        .persistable_code()
        .filter(|code| is_history_failure_code(code))
        .map(|_| ContinuationFailure::HistoryUnavailable);
    let send_state = upstream_send_state(failure.send_phase);
    let mut error = provider_error(provider_error_kind(category), send_state)
        .redact_sensitive_context("upstream response body");
    if let Some(status) = failure.status {
        error = error.with_status(status.as_u16());
    }
    if replay_boundary.permits_provider_proof()
        && (failure.replay_is_safe() || continuation_failure.is_some())
    {
        error = error.with_replay_safe();
    }
    if let Some(continuation_failure) = continuation_failure {
        error = error.with_continuation_failure(continuation_failure);
    }
    if let Some(retry_after) = failure.retry_after_seconds.map(Duration::from_secs) {
        error = error.with_retry_after(retry_after);
    }
    if let Some(code) = failure
        .persistable_code()
        .and_then(|code| SafeUpstreamValue::new(code.to_owned()).ok())
    {
        error = error.with_upstream_code(code);
    }
    if let Some(request_id) = failure
        .request_id
        .as_deref()
        .and_then(|request_id| SafeUpstreamValue::new(request_id.to_owned()).ok())
    {
        error = error.with_upstream_request_id(request_id);
    }
    MappedProviderFailure {
        error,
        account_failure: account_failure(category, failure.retry_after_seconds),
        set_cookie_headers: failure.set_cookie_headers,
        rate_limit_headers: failure.rate_limit_headers,
        observation,
        capture_response_cookies: !matches!(
            category,
            CodexFailureCategory::CloudflareChallenge | CodexFailureCategory::CloudflarePathBlocked
        ),
    }
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
        | CodexWebSocketExchangeError::InvalidCompletedResponse { .. }
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
        | CodexWebSocketExchangeError::InvalidCompletedResponse { .. }
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
const QUOTA_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const DESKTOP_RELEASE_WORKER_OWNER: &str = "openai-desktop-release";
const CLI_RELEASE_WORKER_OWNER: &str = "openai-cli-release";
const MODEL_ETAG_WORKER_OWNER: &str = "openai-model-etag";

pub(crate) fn worker_contributions(
    refresh: Arc<CodexCredentialRefreshService>,
    quota: Arc<CodexCredentialQuotaService>,
    catalog: Arc<CodexCredentialCatalogService>,
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
    Ok(vec![
        WorkerContribution::Registration(scheduled_registration(
            refresh_id,
            OAUTH_REFRESH_INTERVAL,
            Box::new(OpenAiOAuthRefreshTask { service: refresh }),
        )?),
        WorkerContribution::Registration(scheduled_registration(
            quota_id,
            QUOTA_REFRESH_INTERVAL,
            Box::new(OpenAiQuotaTask { quota }),
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
    ])
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
                            | CodexCredentialRefreshOutcome::Ambiguous { .. }
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
            match self.quota.synchronize().await {
                Ok(summary) if summary.has_operational_failures() => {
                    tracing::warn!("OpenAI quota cycle contained operational failures");
                    Ok(())
                }
                Ok(_) => Ok(()),
                Err(_) => Err(WorkerTaskError::safe("OpenAI quota synchronization failed")),
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
