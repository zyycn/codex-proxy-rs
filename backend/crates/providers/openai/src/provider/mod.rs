//! Codex 的 `gateway-core` Provider adapter。

use std::collections::BTreeSet;
use std::fmt;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{StreamExt, future::BoxFuture};
use gateway_core::account::{AccountFeedbackStats, ProviderAccount};
use gateway_core::engine::continuation::{ContinuationBinding, NativeContinuationScope};
use gateway_core::engine::provider::{
    ContinuationRequestObservation, EventStream, Provider, ProviderCallMetadata,
    ProviderCatalogGeneration, ProviderModelCapabilities, ProviderRequest,
    ProviderRequestObservation, ProviderSelectionObservation, ProviderStream,
};
use gateway_core::engine::{AttemptContext, AttemptTransport, ContinuationAttempt};
use gateway_core::error::{
    ClientVisibleUpstreamError, ClientVisibleUpstreamResponse, ContinuationFailure,
    ContinuationRecoveryDisposition, OpaqueUpstreamValue, ProviderConnectionObservation,
    ProviderDiagnostic, ProviderError, ProviderErrorKind, RawUpstreamError,
};
use gateway_core::event::{
    FinishReason, GatewayEvent, ProtocolWireEvent, ProviderEvent, ProviderResponseHeader,
    ProviderResponseMetadata, ProviderResponseObservation, ProviderResponseTimings, ResponseMeta,
    UpstreamHttpVersion, WebSocketPoolKind,
};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::operation::{
    GenerateRequest, ImageRequest, ImageRequestKind, Operation, OperationKind,
    ProviderSessionState, StandaloneSearchRequest,
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
use gateway_core::upstream::{UpstreamSendState, UpstreamTransport};
use gateway_protocol::openai::events::{
    ParsedRateLimits, parse_rate_limit_headers, rate_limits_to_header_pairs,
};
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use url::Url;
use uuid::Uuid;

use crate::credential::{
    CodexAccountFailure, CodexCredentialCatalogError, CodexCredentialCatalogService,
    CodexCredentialLease, CodexCredentialQuotaService, CodexCredentialRefreshOutcome,
    CodexCredentialRefreshService, CodexCredentialSelector, CodexCyberPolicyScope,
    CodexQuotaRefreshPolicy, CodexSessionAffinity, CredentialSelectionError, RuntimeCodexCookie,
    SelectCodexCredential, SelectCodexProviderEndpointCredential,
    derive_codex_cyber_policy_session_key, derive_codex_session_affinity,
    derive_previous_response_id_hash,
};
use crate::session_transport::{
    CodexSessionRecoveryTransition, CodexSessionTransportDecision, CodexSessionTransportRecovery,
    CodexSessionWebSocketProbe, SessionWebSocketFallback,
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
    CodexResponsesRequest, PREVIOUS_RESPONSE_NOT_FOUND_CODE, PREVIOUS_RESPONSE_NOT_FOUND_MESSAGE,
    PreviousResponseScope, ResponseEventSignals, TransportRequirement, transport_requirement,
};
use crate::transport::protocol::websocket::WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE;
use crate::transport::request::{
    CodexRequestEncodeError, RequestAccountScope, encode_generate_request, scope_request_to_account,
};
use crate::transport::session::CodexSessionIdentity;
use crate::transport::usage::normalize_service_tier;
use crate::transport::websocket::{CodexWebSocketExchangeError, PreviousResponseUnavailableReason};
use crate::transport::{
    CODEX_ALPHA_SEARCH_PATH, CODEX_IMAGE_EDITS_PATH, CODEX_IMAGE_GENERATIONS_PATH,
    CODEX_RESPONSES_PATH, CodexAccountSelectionTelemetry, CodexBackendClient,
    CodexBackendJsonResponse, CodexBackendStreamingResponse, CodexBackendTransport,
    CodexClientError, CodexRateLimitUpdates, CodexRequestContext, CodexResponseMetadata,
    CodexTransportMetrics, CodexUpstreamDiagnostics, CodexWebSocketPool,
    WebSocketConnectionPreference, endpoint_url,
};

mod execution;
mod failure;
mod observation;
mod workers;

use execution::*;
#[doc(hidden)]
pub use failure::openai_failure_affects_account_score;
use failure::*;
use observation::*;
pub(crate) use workers::worker_contributions;

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
    search_url: Url,
    session_identity: Option<CodexSessionIdentity>,
    session_transport_recovery: CodexSessionTransportRecovery,
    stream_max_retries: u32,
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
        stream_max_retries: u32,
    ) -> Result<Self, CodexProviderConfigError> {
        let responses_url = Url::parse(&endpoint_url(&base_url, CODEX_RESPONSES_PATH))
            .map_err(|_| CodexProviderConfigError::InvalidBaseUrl)?;
        let image_generations_url =
            Url::parse(&endpoint_url(&base_url, CODEX_IMAGE_GENERATIONS_PATH))
                .map_err(|_| CodexProviderConfigError::InvalidBaseUrl)?;
        let image_edits_url = Url::parse(&endpoint_url(&base_url, CODEX_IMAGE_EDITS_PATH))
            .map_err(|_| CodexProviderConfigError::InvalidBaseUrl)?;
        let search_url = Url::parse(&endpoint_url(&base_url, CODEX_ALPHA_SEARCH_PATH))
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
            search_url,
            session_identity: None,
            session_transport_recovery: CodexSessionTransportRecovery::default(),
            stream_max_retries,
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

    fn request_observation(
        &self,
        operation: &Operation,
        client_api_key_id: &gateway_core::policy::ClientApiKeyId,
    ) -> ProviderRequestObservation {
        let Operation::Generate(request) = operation else {
            return ProviderRequestObservation::default();
        };
        let Ok(encoded) = encode_generate_request(request, "observability") else {
            return ProviderRequestObservation::default();
        };
        let semantics = encoded.semantics();
        let reasoning_effort = semantics.reasoning_effort.clone();
        let previous_response_id = encoded.previous_response_id();
        let continuation = ContinuationRequestObservation {
            affinity_hash: derive_codex_session_affinity(&encoded, client_api_key_id)
                .map(|affinity| affinity.persistence_hash().to_owned()),
            previous_response_id_hash: previous_response_id.map(|response_id| {
                derive_previous_response_id_hash(response_id, client_api_key_id)
            }),
            requested: previous_response_id.is_some(),
        };
        ProviderRequestObservation {
            reasoning_effort,
            reasoning_preset: semantics.reasoning_preset.map(str::to_owned),
            request_kind: semantics.request_kind,
            subagent_kind: semantics.subagent_kind,
            compact: semantics.compact,
            continuation,
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
        if let Operation::Search(search) = request.operation() {
            return self.execute_search(search, candidate, context).await;
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

        let selection_started_at = Instant::now();
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
        let account_selection_wait_ms =
            u64::try_from(selection_started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
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
                    let native_scope = match previous_session
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
                            NativeContinuationScope::Persisted => PreviousResponseScope::Persisted,
                            NativeContinuationScope::ConnectionLocal => {
                                PreviousResponseScope::ConnectionLocal
                            }
                        },
                    };
                    if matches!(
                        context.continuation_attempt(),
                        ContinuationAttempt::ReplayOwner | ContinuationAttempt::ReplayAny
                    ) && native_scope == PreviousResponseScope::ConnectionLocal
                    {
                        tracing::warn!(
                            request_id = context.request_id().as_str(),
                            attempt_index = context.attempt_index().get(),
                            continuation_scope = "connection_local",
                            continuation_attempt = context.continuation_attempt().as_str(),
                            continuation_recovery_disposition = "client_replay_required",
                            continuation_recovery_action = "stop_proxy_recovery",
                            "OpenAI connection-local continuation replay was rejected before send"
                        );
                        return Err(continuation_replay_required_error("scope_unavailable"));
                    }
                    let previous_response_scope = match context.continuation_attempt() {
                        ContinuationAttempt::Native => native_scope,
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
            return Err(continuation_replay_required_error("scope_unavailable"));
        }
        scope_request_to_account(
            &mut upstream_request,
            lease.installation_id(),
            account_scope,
        );
        let requirement = transport_requirement(&upstream_request);
        let requested_transport = selected_transport(&upstream_request);
        let session_transport_decision = if matches!(
            context.transport(),
            AttemptTransport::Default | AttemptTransport::Retry(_)
        ) && requested_transport
            == CodexProviderTransport::PreferWebSocket
            && requirement.allows_session_transport_recovery()
            && let Some(affinity) = session_affinity.as_ref()
        {
            self.session_transport_recovery.decide(affinity.key())
        } else {
            CodexSessionTransportDecision::Default
        };
        let session_transport_action = session_transport_decision.action();
        let session_websocket_failure_count = session_transport_decision.failure_count();
        let session_http_cooldown_ms = session_transport_decision
            .retry_after()
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX));
        let mut websocket_connection_preference = WebSocketConnectionPreference::ReuseOrConnect;
        let mut session_recovery_probe = None;
        let transport = if requirement.requires_websocket() {
            CodexProviderTransport::PreferWebSocket
        } else {
            match context.transport() {
                AttemptTransport::Default | AttemptTransport::Retry(_) => {
                    match session_transport_decision {
                        CodexSessionTransportDecision::Default => requested_transport,
                        CodexSessionTransportDecision::FreshWebSocket { probe, .. } => {
                            websocket_connection_preference = WebSocketConnectionPreference::Fresh;
                            session_recovery_probe = Some(probe);
                            CodexProviderTransport::PreferWebSocket
                        }
                        CodexSessionTransportDecision::HttpSse { .. } => {
                            CodexProviderTransport::HttpOnly
                        }
                    }
                }
                AttemptTransport::Fallback => CodexProviderTransport::HttpOnly,
            }
        };
        if let Some(session_transport_action) = session_transport_action {
            tracing::info!(
                request_id = %context.request_id(),
                attempt_index = context.attempt_index().get(),
                session_affinity_key_hash = session_affinity
                    .as_ref()
                    .map_or("", CodexSessionAffinity::key_hash),
                transport_requirement = requirement.as_str(),
                session_transport_action,
                session_websocket_failure_count = session_websocket_failure_count.unwrap_or_default(),
                session_http_cooldown_ms = session_http_cooldown_ms.unwrap_or_default(),
                session_http_cooldown_present = session_http_cooldown_ms.is_some(),
                "Applied OpenAI session transport recovery decision"
            );
        }
        apply_transport(&mut upstream_request, transport);
        let metadata = ProviderCallMetadata::new(
            provider_kind,
            upstream_model.clone(),
            lease.account_id().clone(),
            UpstreamTransport::new(transport_name(transport)).map_err(|_| {
                provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
            })?,
        )
        .with_selection_observation(ProviderSelectionObservation::new(
            account_selection_wait_ms,
            lease.capacity_snapshot(),
        ));
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
        let session_affinity_key_hash = session_affinity
            .as_ref()
            .map(|affinity| affinity.key_hash().to_owned());
        let session_affinity_key = session_affinity.map(CodexSessionAffinity::into_key);
        let websocket_retry_count = match context.transport() {
            AttemptTransport::Retry(retry_index) => retry_index.get(),
            AttemptTransport::Default | AttemptTransport::Fallback => 0,
        };
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
            session_affinity_key_hash,
            websocket_connection_preference,
            session_transport_recovery: self.session_transport_recovery.clone(),
            session_recovery_probe,
            websocket_retry_count,
            stream_max_retries: self.stream_max_retries,
            session_capture,
        });
        let stream = ProviderStream::new(metadata, events, lease);
        Ok(if allows_account_state_mutation {
            stream.with_filtered_account_feedback(
                Arc::clone(&self.account_feedback),
                openai_failure_affects_account_score,
            )
        } else {
            stream
        })
    }
}
