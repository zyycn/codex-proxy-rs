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
use gateway_core::engine::middleware::MiddlewareHeader;
use gateway_core::engine::provider::{
    ContinuationRequestObservation, EventStream, Provider, ProviderCallMetadata, ProviderRequest,
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
    CapabilityRequirements, GenerateRequest, ImageRequest, ImageRequestKind, Operation,
    OperationKind, ProviderSessionState, StandaloneSearchRequest,
};
use gateway_core::provider_ports::ProviderSessionAffinityKey;
use gateway_core::routing::{
    ModelCapabilities, ModelPresentation, ProviderCandidate, ProviderCatalogGeneration,
    ProviderKind, ProviderModelCapabilities, UpstreamModelId,
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
use reqwest::{Client, header::HeaderValue};
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
    derive_codex_cyber_policy_session_key, derive_codex_endpoint_session_affinity,
    derive_codex_session_affinity, derive_previous_response_id_hash,
};
use crate::session_transport::CodexSessionTransportRecovery;
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
    CodexRequestEncodeError, RequestAccountScope, align_structured_location_fields,
    encode_generate_request, scope_request_to_account,
};
use crate::transport::session::CodexSessionIdentity;
use crate::transport::usage::normalize_service_tier;
use crate::transport::websocket::{
    CodexWebSocketExchangeError, PreviousResponseUnavailableReason, WEBSOCKET_CLOSE_MESSAGE_TOO_BIG,
};
use crate::transport::{
    CODEX_ALPHA_SEARCH_PATH, CODEX_IMAGE_EDITS_PATH, CODEX_IMAGE_GENERATIONS_PATH,
    CODEX_RESPONSES_PATH, CodexAccountSelectionTelemetry, CodexBackendClient,
    CodexBackendJsonResponse, CodexBackendStreamingResponse, CodexBackendTransport,
    CodexClientError, CodexRateLimitUpdates, CodexRequestContext, CodexResponseMetadata,
    CodexResponseMetadataUpdates, CodexTransportMetrics, CodexUpstreamDiagnostics,
    CodexWebSocketPool, endpoint_url, normalize_selected_codex_downstream_body,
};

mod execution;
mod failure;
mod observation;
mod workers;
pub(crate) use workers::ClientReleaseServices;

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

#[derive(Clone)]
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

struct PreparedGenerateRequest {
    upstream: CodexResponsesRequest,
    previous_session: Option<OpenAiSessionState>,
    continuation_requested: bool,
    session_affinity: Option<CodexSessionAffinity>,
    cyber_policy_session_key: Option<ProviderSessionAffinityKey>,
}

struct SelectedGenerate {
    lease: CodexCredentialLease,
    session_affinity: Option<CodexSessionAffinity>,
    cyber_policy_key: Option<ProviderSessionAffinityKey>,
    account_selection_wait_ms: u64,
    frozen_requirements: CapabilityRequirements,
}

impl CodexProvider {
    fn prepare_generate_request(
        &self,
        generate: &GenerateRequest,
        upstream_model: &UpstreamModelId,
        context: &AttemptContext,
    ) -> Result<PreparedGenerateRequest, ProviderError> {
        let previous_session = decode_openai_session_state(generate);
        let continuation_requested = generate.native_continuation_requested();
        let mut upstream = encode_generate_request(generate, upstream_model.as_str(), None)
            .map_err(map_request_error)?;
        // 插件加工后仍重新应用宿主强制策略与本地会话身份。
        upstream.apply_fast_policy(context.disable_fast());
        if let Some(conversation_id) = previous_session
            .as_ref()
            .and_then(|state| state.conversation_id.as_ref())
        {
            upstream.local_conversation_id = Some(conversation_id.clone());
        }
        if let Some(identity) = &self.session_identity {
            identity.prepare_local_conversation(&mut upstream);
        }
        if let Some(previous_session) = previous_session.as_ref() {
            upstream.turn_state = if same_client_turn(
                previous_session.client_turn_id.as_deref(),
                upstream.client_turn_id.as_deref(),
            ) {
                upstream
                    .turn_state
                    .take()
                    .or_else(|| previous_session.turn_state.clone())
            } else {
                None
            };
        }
        let session_affinity =
            derive_codex_session_affinity(&upstream, context.client_api_key_ref());
        let cyber_policy_session_key =
            derive_codex_cyber_policy_session_key(&upstream, context.client_api_key_ref());
        Ok(PreparedGenerateRequest {
            upstream,
            previous_session,
            continuation_requested,
            session_affinity,
            cyber_policy_session_key,
        })
    }

    fn client_for_request(
        &self,
        context: &AttemptContext,
    ) -> Result<CodexBackendClient, ProviderError> {
        let Some(profile) = context.request_profile() else {
            return Ok(self.client.clone());
        };
        let profile = serde_json::from_value(Value::Object(profile.expose_to_provider().clone()))
            .map_err(|_| {
            provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
        })?;
        Ok(self.client.clone().with_request_profile(profile))
    }

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
    fn resolve_request_profile(
        &self,
        configuration: &gateway_core::account::OpaqueProviderData,
    ) -> Result<gateway_core::account::OpaqueProviderData, ProviderError> {
        let selection =
            crate::transport::profile::identity::RequestProfileSelection::parse(configuration)
                .map_err(|_| {
                    provider_error(
                        ProviderErrorKind::InvalidRequest,
                        UpstreamSendState::NotSent,
                    )
                })?;
        let profile = selection
            .resolve(self.client.profile_state())
            .map_err(|_| {
                provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent)
            })?;
        crate::transport::profile::selection::object(&profile)
            .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent))
    }

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
            let affinity = match operation {
                Operation::Search(request) => derive_codex_endpoint_session_affinity(
                    request.payload(),
                    client_api_key_id,
                    "id",
                ),
                Operation::GenerateImage(request) => derive_codex_endpoint_session_affinity(
                    request.payload(),
                    client_api_key_id,
                    "session_id",
                ),
                _ => None,
            };
            return ProviderRequestObservation {
                requested_model: match operation {
                    Operation::Search(request) => {
                        observation::endpoint_requested_model(request.payload())
                    }
                    Operation::GenerateImage(request) => {
                        observation::endpoint_requested_model(request.payload())
                    }
                    _ => None,
                },
                continuation: ContinuationRequestObservation {
                    affinity_hash: affinity.map(|affinity| affinity.persistence_hash().to_owned()),
                    ..Default::default()
                },
                ..Default::default()
            };
        };
        let Ok(encoded) = encode_generate_request(request, "observability", None) else {
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
            requested_model: None,
            reasoning_effort,
            reasoning_preset: semantics.reasoning_preset.map(str::to_owned),
            request_kind: semantics.request_kind,
            subagent_kind: semantics.subagent_kind,
            compact: semantics.compact,
            continuation,
        }
    }

    fn model_catalog_is_exhaustive(&self) -> bool {
        false
    }

    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        let snapshot = self.catalog.synchronize().await.map_err(|_| {
            provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent)
        })?;
        let mut accounts = snapshot.model_catalog_accounts();
        Ok(snapshot
            .models()
            .iter()
            .map(|model| {
                compile_model_capabilities(model).with_catalog_accounts(
                    accounts
                        .remove(model.request_model().as_str())
                        .unwrap_or_default(),
                )
            })
            .collect())
    }

    async fn query_client_model_catalog(
        &self,
        scope: &gateway_core::account::scope::FrozenAccountScope,
        protocol: &str,
        client_version: &str,
    ) -> Result<
        Option<Vec<gateway_core::routing::ProviderModelDescriptor>>,
        gateway_core::routing::ProviderCatalogUnavailable,
    > {
        if protocol != "codex" {
            return Ok(None);
        }
        self.catalog
            .client_model_catalog(scope, client_version)
            .await
            .map(Some)
            .map_err(|_| gateway_core::routing::ProviderCatalogUnavailable)
    }

    async fn execute(
        self: Arc<Self>,
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
        // 其他协议必须先取得真实账号，再按固定 attempt 阶段调用转换器；选号前不能
        // 把未知正文当成 OpenAI wire 解释会话、亲和或传输字段。
        let preselection = (generate.protocol_payload().protocol() == PROVIDER_NAME)
            .then(|| self.prepare_generate_request(generate, upstream_model, &context))
            .transpose()?;
        let (selection_session_affinity, selection_cyber_policy_key, requires_websocket) =
            preselection.map_or((None, None, false), |prepared| {
                let requires_websocket =
                    transport_requirement(&prepared.upstream).requires_websocket()
                        || (context.continuation_attempt() == ContinuationAttempt::Native
                            && (prepared.previous_session.as_ref().is_some_and(|state| {
                                state.continuation_scope
                                    == OpenAiContinuationScope::ConnectionLocal
                            }) || matches!(context.continuation(), Some(ContinuationBinding::Pinned(binding))
                                    if binding.scope() == NativeContinuationScope::ConnectionLocal)));
                (
                    prepared.session_affinity,
                    prepared.cyber_policy_session_key,
                    requires_websocket,
                )
            });
        let selection_started_at = Instant::now();
        let selection = async {
            self.selector
                .select_with_cyber_policy(
                    &SelectCodexCredential {
                        upstream_model: upstream_model.as_str(),
                        request_url: &self.responses_url,
                        attempt: &context,
                        session_affinity_key: selection_session_affinity
                            .as_ref()
                            .map(|affinity| affinity.key()),
                    },
                    selection_cyber_policy_key.as_ref(),
                    selection_session_affinity.as_ref(),
                    requires_websocket,
                )
                .await
                .map_err(map_selection_error)
        };
        // 恢复期间排队也消耗启动窗口；只包住选账号，不限制业务响应时长。
        let lease = if let Some(remaining) = context.connection_budget().startup_remaining() {
            tokio::time::timeout(remaining, selection)
                .await
                .map_err(|_| {
                    provider_error(ProviderErrorKind::Timeout, UpstreamSendState::NotSent)
                })??
        } else {
            selection.await?
        };
        let account_selection_wait_ms =
            u64::try_from(selection_started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        let provider_kind = ProviderKind::new(PROVIDER_NAME)
            .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent))?;
        let account_id = lease.account_id().clone();
        let frozen_requirements = Operation::Generate(generate.clone()).capability_requirements();
        let provider = Arc::clone(&self);
        let terminal_context = context.clone();
        let terminal_model = upstream_model.clone();
        context
            .execute_middleware(
                Operation::Generate(generate.clone()),
                provider_kind,
                Some(upstream_model.as_str().to_owned()),
                account_id,
                Box::new(move |operation, middleware_headers| {
                    Box::pin(async move {
                        provider
                            .execute_selected_generate(
                                operation,
                                middleware_headers,
                                terminal_model,
                                terminal_context,
                                SelectedGenerate {
                                    lease,
                                    session_affinity: selection_session_affinity,
                                    cyber_policy_key: selection_cyber_policy_key,
                                    account_selection_wait_ms,
                                    frozen_requirements,
                                },
                            )
                            .await
                    })
                }),
            )
            .await
    }
}

impl CodexProvider {
    async fn execute_selected_generate(
        self: Arc<Self>,
        operation: Operation,
        middleware_headers: Vec<MiddlewareHeader>,
        upstream_model: UpstreamModelId,
        context: AttemptContext,
        selected: SelectedGenerate,
    ) -> Result<ProviderStream, ProviderError> {
        let SelectedGenerate {
            mut lease,
            session_affinity: selection_session_affinity,
            cyber_policy_key: selection_cyber_policy_key,
            account_selection_wait_ms,
            frozen_requirements,
        } = selected;
        let Operation::Generate(generate) = operation else {
            return Err(provider_error(
                ProviderErrorKind::Protocol,
                UpstreamSendState::NotSent,
            ));
        };
        if generate.protocol_payload().protocol() != PROVIDER_NAME
            || native_request_requirements(&generate) != frozen_requirements
        {
            return Err(provider_error(
                ProviderErrorKind::InvalidRequest,
                UpstreamSendState::NotSent,
            ));
        }
        validate_openai_reasoning(generate.protocol_payload().body())?;
        let processed = self.prepare_generate_request(&generate, &upstream_model, &context)?;
        let mut upstream_request = processed.upstream;
        let previous_session = processed.previous_session;
        let continuation_requested = processed.continuation_requested;
        let session_affinity = processed.session_affinity;
        let cyber_policy_session_key = processed.cyber_policy_session_key;
        if selection_session_affinity
            .as_ref()
            .map(|affinity| affinity.key())
            != session_affinity.as_ref().map(|affinity| affinity.key())
            || selection_cyber_policy_key.as_ref() != cyber_policy_session_key.as_ref()
        {
            self.selector
                .validate_translated_selection(
                    &mut lease,
                    session_affinity.as_ref(),
                    cyber_policy_session_key.as_ref(),
                )
                .await
                .map_err(map_selection_error)?;
        }
        let lease = Arc::new(lease);
        if previous_session.as_ref().is_some_and(|state| {
            state
                .credential_revision
                .is_some_and(|revision| revision != lease.account().revision().get())
        }) {
            return Err(continuation_replay_required_error("scope_unavailable"));
        }
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
        if matches!(
            lease.authentication(),
            crate::credential::CodexRuntimeAuthentication::OAuth(_)
        ) {
            normalize_selected_codex_downstream_body(
                upstream_request.body_mut(),
                generate.protocol_payload().context(),
            );
        }
        if let Some(location) = lease
            .account()
            .request_location()
            .or(context.request_location())
        {
            align_structured_location_fields(
                upstream_request.body_mut(),
                chrono::Utc::now(),
                location,
            );
        }
        let requirement = transport_requirement(&upstream_request);
        let http_only = lease.transport() == crate::credential::ResponsesTransport::Http;
        if http_only && requirement.requires_websocket() {
            return Err(provider_error(
                ProviderErrorKind::Unsupported,
                UpstreamSendState::NotSent,
            ));
        }
        let requested_transport = if http_only {
            CodexProviderTransport::HttpOnly
        } else {
            selected_transport(&upstream_request)
        };
        let session_http_fallback = requirement.allows_pre_send_http_fallback()
            && session_affinity
                .as_ref()
                .is_some_and(|affinity| self.session_transport_recovery.uses_http(affinity.key()));
        let transport = if requirement.requires_websocket() {
            CodexProviderTransport::PreferWebSocket
        } else if context.transport() == AttemptTransport::Fallback || session_http_fallback {
            CodexProviderTransport::HttpOnly
        } else {
            requested_transport
        };
        if transport == CodexProviderTransport::PreferWebSocket
            && middleware_headers.iter().any(|header| {
                match HeaderValue::from_bytes(header.value()) {
                    Ok(value) => value.to_str().is_err(),
                    Err(_) => true,
                }
            })
        {
            return Err(provider_error(
                ProviderErrorKind::Protocol,
                UpstreamSendState::NotSent,
            ));
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
                credential_revision: matches!(
                    lease.authentication(),
                    crate::credential::CodexRuntimeAuthentication::ApiKey(_)
                )
                .then_some(lease.account().revision().get()),
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
            client: self
                .client_for_request(&context)?
                .for_account(lease.account())
                .map_err(|_| {
                    provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent)
                })?
                .with_authentication(lease.authentication())
                .with_connection_budget(context.connection_budget().clone())
                .with_middleware_headers(middleware_headers),
            response_origin: self.responses_url.clone(),
            request: upstream_request,
            upstream_model,
            transport_policy: transport,
            context,
            selector: Arc::clone(&self.selector),
            quota: Arc::clone(&self.quota),
            catalog: Arc::clone(&self.catalog),
            lease: Arc::clone(&lease),
            output_started_at,
            session_affinity_key,
            session_affinity_key_hash,
            session_transport_recovery: self.session_transport_recovery.clone(),
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

fn native_request_requirements(request: &GenerateRequest) -> CapabilityRequirements {
    // 此处只解释已知 OpenAI wire；不在 Core 的通用转换路径推断任意目标协议。
    Operation::Generate(GenerateRequest::from_protocol_payload(
        request.protocol_payload().clone(),
    ))
    .capability_requirements()
}

fn validate_openai_reasoning(body: &Map<String, Value>) -> Result<(), ProviderError> {
    let Some(effort) = body
        .get("reasoning")
        .and_then(Value::as_object)
        .and_then(|reasoning| reasoning.get("effort"))
    else {
        return Ok(());
    };
    let Some(effort) = effort.as_str() else {
        return Err(provider_error(
            ProviderErrorKind::InvalidRequest,
            UpstreamSendState::NotSent,
        ));
    };
    if effort.is_empty() || effort.len() > 64 || effort.chars().any(char::is_control) {
        return Err(provider_error(
            ProviderErrorKind::InvalidRequest,
            UpstreamSendState::NotSent,
        ));
    }
    Ok(())
}
