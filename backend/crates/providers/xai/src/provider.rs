//! 官方 Grok Build 会话的 `gateway-core` Provider adapter。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use futures::{StreamExt, future::BoxFuture};
use gateway_core::engine::continuation::ContinuationBinding;
use gateway_core::engine::credential::{
    AccountAvailability, AccountAvailabilityPolicy, AccountFeedbackStats, ProviderAccount,
    ProviderAccountId, ProviderAccountStore,
};
use gateway_core::engine::provider::{
    EventStream, Provider, ProviderCallMetadata, ProviderCatalogGeneration,
    ProviderModelCapabilities, ProviderRequest, ProviderRequestObservation, ProviderStream,
    UpstreamTransport,
};
use gateway_core::engine::{AttemptContext, ContinuationAttempt, UpstreamSendState};
use gateway_core::error::{
    ClientVisibleUpstreamError, ContinuationFailure, ProviderError, ProviderErrorKind,
};
use gateway_core::event::{
    GatewayEvent, ProviderEvent, ProviderResponseMetadata, ProviderResponseObservation,
    ProviderResponseTimings, ResponseMeta,
};
use gateway_core::operation::{
    Feature, GenerateRequest, Operation, OperationKind, ProviderSessionState,
};
use gateway_core::routing::{
    ModelCapabilities, ModelPresentation, ProviderCandidate, ProviderKind, SupportLevel,
    UpstreamModelId,
};
use gateway_core::task::{
    ScheduledTask, WorkerContribution, WorkerCycleContext, WorkerDefinitionError, WorkerId,
    WorkerKind, WorkerLeaseRequest, WorkerRegistration, WorkerRunnable, WorkerSchedule,
    WorkerTaskError,
};
use gateway_protocol::openai::codex_responses_request_semantics;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::Url;

use crate::XaiWireProfileState;
use crate::credential::{
    GrokCredentialCatalogError, GrokCredentialCatalogService, GrokCredentialQuotaService,
    GrokCredentialRecovery, GrokCredentialRecoveryOutcome, GrokCredentialRefreshOutcome,
    GrokCredentialRefreshService, GrokQuotaError,
};
use crate::reasoning_replay::{
    GrokReasoningReplay, GrokReasoningReplayCapture, GrokReasoningReplayKey,
    valid_reasoning_ciphertext,
};
use crate::transport::canonical::GrokCanonicalDecoder;
use crate::transport::config::XAI_PROVIDER_NAME;
use crate::transport::headers::{GrokClientIdentity, build_grok_headers};
use crate::transport::profile::{GROK_CLI_RELEASE_POLL_INTERVAL, GrokCliReleaseService};
use crate::transport::{
    GROK_RESPONSES_URL, GrokCompactionDecodeError, GrokCompactionRequest,
    GrokCompactionSummaryDecoder, GrokCredentialFailure, GrokInferenceChunkStream,
    GrokInferenceRequest, GrokInferenceResponse, GrokInferenceTransport,
    GrokInferenceTransportError, GrokInferenceTransportErrorKind, GrokInferenceTransportMetrics,
    GrokProviderConfigError, GrokQuotaFailureKind, GrokRequestEncodeError, GrokResponsesRequest,
    GrokSessionAffinityKey, GrokSessionSelection, GrokSessionSelector, GrokSessionSelectorError,
    SelectedGrokSession, classify_grok_quota_failure,
};
use crate::{GrokCatalogCapabilityEvidence, GrokCatalogModel};

const HTTP_SSE_TRANSPORT: &str = "http_sse";
const DEFAULT_GROK_MODEL: &str = "grok-4.5";
const XAI_SESSION_STATE_MAX_BYTES: usize = 8 * 1024 * 1024;
const XAI_SESSION_OUTPUT_LIMIT: usize = 4_096;
const REASONING_DECODE_FAILED_CODE: &str = "reasoning_decode_failed";
const RESPONSE_NOT_FOUND_CODE: &str = "not_found";

/// 官方 Grok Build Provider；会话选择与 HTTP SSE transport 均由外部注入。
///
/// 每次调用只选择一个 OAuth 会话，只准备一次可见的上游 POST。重试、凭据轮换、
/// endpoint fallback 以及公开 xAI API key 推理都不在该 adapter 内。
pub struct GrokBuildProvider {
    selector: Arc<dyn GrokSessionSelector>,
    transport: Arc<dyn GrokInferenceTransport>,
    catalog: Arc<GrokCredentialCatalogService>,
    credential_recovery: Arc<dyn GrokCredentialRecovery>,
    account_feedback: Arc<AccountFeedbackStats>,
    client_identity: GrokClientIdentity,
    reasoning_replay: GrokReasoningReplay,
    wire_profile: XaiWireProfileState,
    responses_url: Url,
}

impl GrokBuildProvider {
    /// 在显式的会话与 transport 边界上创建 Provider。
    pub fn new(
        selector: Arc<dyn GrokSessionSelector>,
        transport: Arc<dyn GrokInferenceTransport>,
        catalog: Arc<GrokCredentialCatalogService>,
        credential_recovery: Arc<dyn GrokCredentialRecovery>,
        account_feedback: Arc<AccountFeedbackStats>,
        wire_profile: XaiWireProfileState,
    ) -> Result<Self, GrokProviderConfigError> {
        let responses_url = Url::parse(GROK_RESPONSES_URL)
            .map_err(|_| GrokProviderConfigError::InvalidResponsesUrl)?;
        Ok(Self {
            selector,
            transport,
            catalog,
            credential_recovery,
            account_feedback,
            client_identity: GrokClientIdentity::new(),
            reasoning_replay: GrokReasoningReplay::new(),
            wire_profile,
            responses_url,
        })
    }
}

#[async_trait]
impl Provider for GrokBuildProvider {
    fn name(&self) -> &'static str {
        XAI_PROVIDER_NAME
    }

    fn catalog_generation(&self) -> ProviderCatalogGeneration {
        self.catalog.catalog_generation()
    }

    fn request_observation(&self, operation: &Operation) -> ProviderRequestObservation {
        let Operation::Generate(request) = operation else {
            return ProviderRequestObservation::default();
        };
        let payload = request.protocol_payload();
        if payload.protocol() != "openai" {
            return ProviderRequestObservation::default();
        }
        let semantics = codex_responses_request_semantics(payload.body(), payload.context());
        ProviderRequestObservation {
            reasoning_effort: semantics.reasoning_effort,
            reasoning_preset: semantics.reasoning_preset.map(str::to_owned),
            request_kind: semantics.request_kind,
            subagent_kind: semantics.subagent_kind,
            compact: semantics.compact,
        }
    }

    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        let Ok(models) = self.catalog.query_models().await else {
            return Ok(vec![default_grok_model_capabilities()?]);
        };
        if models.is_empty() {
            return Ok(vec![default_grok_model_capabilities()?]);
        }
        Ok(models
            .into_iter()
            .map(compile_grok_model_capabilities)
            .collect())
    }

    async fn execute(
        &self,
        request: ProviderRequest,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        let candidate = request.candidate();
        if candidate.provider().as_str() != XAI_PROVIDER_NAME {
            return Err(provider_error(
                ProviderErrorKind::InvalidRequest,
                UpstreamSendState::NotSent,
            ));
        }
        preflight_context(&context)?;

        match request.operation() {
            Operation::Generate(generate) => {
                self.execute_generate(generate, candidate, context).await
            }
            _ => Err(provider_error(
                ProviderErrorKind::Unsupported,
                UpstreamSendState::NotSent,
            )),
        }
    }
}

impl GrokBuildProvider {
    async fn execute_generate(
        &self,
        generate: &GenerateRequest,
        candidate: &ProviderCandidate,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        if crate::transport::compaction::has_terminal_compaction_trigger(generate) {
            return self.execute_compaction(generate, candidate, context).await;
        }
        let previous_session = decode_xai_session_state(generate)?;
        let continuation_account = continuation_account(&context, previous_session.as_ref())?;
        let mut upstream_request = GrokResponsesRequest::encode(
            generate,
            candidate.upstream_model().as_str(),
            context.client_api_key_ref(),
        )
        .map_err(map_request_error)?;
        let request_input = upstream_request.input_items();
        if let Some(previous) = previous_session.as_ref() {
            upstream_request.inherit_session(previous.session_id.as_deref());
        }
        let selected = select_grok_session(
            self.selector.as_ref(),
            candidate,
            &context,
            continuation_account,
            upstream_request.affinity().cloned(),
        )
        .await?;
        // 首字计时的起点：账号选择完成之后、上游建立之前。
        let output_started_at = Instant::now();
        apply_continuation(
            &mut upstream_request,
            previous_session.as_ref(),
            &context,
            selected.account_id(),
            request_input.as_slice(),
        )?;
        let inherited_replay_session_id = context
            .continuation()
            .is_none()
            .then(|| {
                previous_session
                    .as_ref()
                    .and_then(|state| state.session_id.as_deref())
            })
            .flatten();
        let reasoning_replay_key = upstream_request
            .reasoning_replay_session_id()
            .or(inherited_replay_session_id)
            .and_then(|session_id| {
                self.reasoning_replay.key(
                    candidate.upstream_model().as_str(),
                    session_id,
                    selected.account_id().as_str(),
                )
            });
        if !upstream_request.has_previous_response_id()
            && let (Some(key), Some(input)) = (
                reasoning_replay_key.as_ref(),
                upstream_request.replay_input_items(),
            )
            && let Some(input) = self.reasoning_replay.apply(key, &input)
        {
            upstream_request
                .set_replay_input(input)
                .map_err(map_request_error)?;
        }
        let reasoning_replay_capture =
            reasoning_replay_key.map(|key| self.reasoning_replay.capture(key));
        let session_capture = (!matches!(
            context.continuation(),
            Some(ContinuationBinding::Pinned(_) | ContinuationBinding::External(_))
        ) || previous_session.is_some())
        .then(|| GrokSessionCapture {
            previous: previous_session,
            request_input,
            account_id: selected.account_id().as_str().to_owned(),
            session_id: upstream_request.session_id().map(str::to_owned),
            output_items: BTreeMap::new(),
        });
        let selected = Arc::new(selected);
        let allows_account_state_mutation = selected.allows_account_state_mutation();
        let metadata = provider_call_metadata(candidate, &selected)?;
        let events = cold_http_sse_stream(
            Arc::clone(&self.selector),
            Arc::clone(&self.transport),
            GrokStreamAttempt {
                client_identity: self.client_identity.clone(),
                wire_profile: self.wire_profile.clone(),
                credential_recovery: Arc::clone(&self.credential_recovery),
                responses_url: self.responses_url.clone(),
                request: upstream_request,
                upstream_model: candidate.upstream_model().clone(),
                context,
                session: Arc::clone(&selected),
                output_started_at,
                session_capture,
                reasoning_replay_capture,
            },
        );
        let stream = ProviderStream::new(metadata, events, selected);
        Ok(if allows_account_state_mutation {
            stream.with_account_feedback(Arc::clone(&self.account_feedback))
        } else {
            stream
        })
    }

    async fn execute_compaction(
        &self,
        generate: &GenerateRequest,
        candidate: &ProviderCandidate,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        if context.continuation().is_some()
            || context.continuation_attempt() != ContinuationAttempt::None
        {
            return Err(provider_error(
                ProviderErrorKind::InvalidRequest,
                UpstreamSendState::NotSent,
            ));
        }
        let previous_session = decode_xai_session_state(generate)?;
        let operation_account = previous_session
            .as_ref()
            .map(|previous| ProviderAccountId::new(previous.account_id.clone()))
            .transpose()
            .map_err(|_| protocol_not_sent())?;
        let inherited_session_id = previous_session
            .as_ref()
            .and_then(|previous| previous.session_id.clone());
        let upstream_request = GrokCompactionRequest::encode(
            generate,
            candidate.upstream_model().as_str(),
            context.client_api_key_ref(),
        )
        .map_err(map_request_error)?;
        let explicit_replay_session_id = upstream_request
            .reasoning_replay_session_id()
            .map(str::to_owned);
        let upstream_session_id = inherited_session_id
            .clone()
            .or_else(|| explicit_replay_session_id.clone());
        let selected = Arc::new(
            select_grok_session(
                self.selector.as_ref(),
                candidate,
                &context,
                operation_account,
                upstream_request.affinity().cloned(),
            )
            .await?,
        );
        let reasoning_replay_key = explicit_replay_session_id
            .as_deref()
            .or(inherited_session_id.as_deref())
            .and_then(|session_id| {
                self.reasoning_replay.key(
                    candidate.upstream_model().as_str(),
                    session_id,
                    selected.account_id().as_str(),
                )
            });
        let allows_account_state_mutation = selected.allows_account_state_mutation();
        let metadata = provider_call_metadata(candidate, &selected)?;
        let events = cold_compaction_http_sse_stream(
            Arc::clone(&self.selector),
            Arc::clone(&self.transport),
            GrokCompactionStreamAttempt {
                client_identity: self.client_identity.clone(),
                wire_profile: self.wire_profile.clone(),
                credential_recovery: Arc::clone(&self.credential_recovery),
                responses_url: self.responses_url.clone(),
                request: upstream_request,
                upstream_model: candidate.upstream_model().clone(),
                upstream_session_id,
                context,
                session: Arc::clone(&selected),
                reasoning_replay: self.reasoning_replay.clone(),
                reasoning_replay_key,
            },
        );
        let stream = ProviderStream::new(metadata, events, selected);
        Ok(if allows_account_state_mutation {
            stream.with_account_feedback(Arc::clone(&self.account_feedback))
        } else {
            stream
        })
    }
}

async fn select_grok_session(
    selector: &dyn GrokSessionSelector,
    candidate: &ProviderCandidate,
    context: &AttemptContext,
    operation_account: Option<ProviderAccountId>,
    affinity: Option<GrokSessionAffinityKey>,
) -> Result<SelectedGrokSession, ProviderError> {
    let required_account = context.required_account().cloned().or(operation_account);
    let selection = GrokSessionSelection::new(
        candidate.upstream_model().clone(),
        context.excluded_accounts().clone(),
        required_account.clone(),
        context.account_selection_policy(),
        context.deadline(),
    )
    .with_availability_policy(if context.is_diagnostic_required_account() {
        AccountAvailabilityPolicy::BypassForDiagnostic
    } else {
        AccountAvailabilityPolicy::Enforce
    })
    .with_affinity(affinity);
    let selection_deadline = remaining(context.deadline())
        .ok_or_else(|| provider_error(ProviderErrorKind::Timeout, UpstreamSendState::NotSent))?;
    let cancellation = context.cancellation().clone();
    let selected = tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(provider_error(
            ProviderErrorKind::Cancelled,
            UpstreamSendState::NotSent,
        )),
        _ = tokio::time::sleep(selection_deadline) => Err(provider_error(
            ProviderErrorKind::Timeout,
            UpstreamSendState::NotSent,
        )),
        selected = selector.select(selection) => selected.map_err(map_selection_error),
    }?;
    if context.excluded_accounts().contains(selected.account_id())
        || required_account
            .as_ref()
            .is_some_and(|required| required != selected.account_id())
    {
        return Err(provider_error(
            ProviderErrorKind::Protocol,
            UpstreamSendState::NotSent,
        ));
    }
    Ok(selected)
}

fn provider_call_metadata(
    candidate: &ProviderCandidate,
    selected: &SelectedGrokSession,
) -> Result<ProviderCallMetadata, ProviderError> {
    Ok(ProviderCallMetadata::new(
        ProviderKind::new(XAI_PROVIDER_NAME).map_err(|_| protocol_not_sent())?,
        candidate.upstream_model().clone(),
        selected.resource(),
        UpstreamTransport::new(HTTP_SSE_TRANSPORT).map_err(|_| protocol_not_sent())?,
    ))
}

fn support(evidence: GrokCatalogCapabilityEvidence) -> SupportLevel {
    match evidence {
        GrokCatalogCapabilityEvidence::DeclaredNative => SupportLevel::Native,
        GrokCatalogCapabilityEvidence::DeclaredUnsupported => SupportLevel::Unsupported,
        GrokCatalogCapabilityEvidence::Unknown => SupportLevel::Unknown,
    }
}

fn tool_support(evidence: GrokCatalogCapabilityEvidence) -> SupportLevel {
    match evidence {
        GrokCatalogCapabilityEvidence::DeclaredNative => SupportLevel::Native,
        GrokCatalogCapabilityEvidence::DeclaredUnsupported => SupportLevel::Unsupported,
        // catalog 省略该可选字段时，Grok Build 的 Responses 工具协议仍可用；
        // 请求 adapter 会在发送前规范化仅客户端侧的工具结构。
        GrokCatalogCapabilityEvidence::Unknown => SupportLevel::Emulated,
    }
}

fn compile_grok_model_capabilities(model: GrokCatalogModel) -> ProviderModelCapabilities {
    let mut operations = BTreeSet::new();
    if model.capabilities().responses_api() == GrokCatalogCapabilityEvidence::DeclaredNative {
        operations.insert(OperationKind::Generate);
    }
    let capabilities = ModelCapabilities::new(
        operations,
        model
            .limits()
            .max_output_tokens()
            .map(std::num::NonZeroU64::get),
    )
    .with_upstream_feature_validation()
    .with_feature(
        Feature::Reasoning,
        support(model.capabilities().reasoning_effort()),
    )
    .with_feature(
        Feature::Tools,
        tool_support(model.capabilities().streaming_tool_calls()),
    )
    .with_feature(Feature::Vision, SupportLevel::Unknown)
    .with_feature(Feature::JsonSchema, SupportLevel::Unknown)
    .with_feature(Feature::NativeContinuation, SupportLevel::Native);
    ProviderModelCapabilities::new(model.request_model().clone(), capabilities)
        .with_presentation(grok_model_presentation(&model))
}

fn default_grok_model_capabilities() -> Result<ProviderModelCapabilities, ProviderError> {
    let model = UpstreamModelId::new(DEFAULT_GROK_MODEL.to_owned())
        .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent))?;
    let capabilities = ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), None)
        .with_upstream_feature_validation();
    Ok(ProviderModelCapabilities::new(model, capabilities)
        .with_presentation(default_grok_model_presentation()))
}

fn default_grok_model_presentation() -> ModelPresentation {
    ModelPresentation::new(
        Some("Grok 4.5".to_owned()),
        Some("xAI Grok 4.5 frontier model with reasoning and vision.".to_owned()),
    )
    .with_context_window_tokens(Some(500_000))
    .with_image_input(true)
    .with_agent_tools(true, true)
}

fn grok_model_presentation(model: &GrokCatalogModel) -> ModelPresentation {
    let slug = model.request_model().as_str();
    let known_grok_4_5 = is_known_grok_4_5_model(slug);
    let reasoning_evidence = model.capabilities().reasoning_effort();
    let catalog_reasoning_efforts = model
        .capabilities()
        .reasoning_efforts()
        .iter()
        .map(|effort| effort.as_str().to_owned())
        .collect::<Vec<_>>();
    let catalog_default_reasoning = model
        .capabilities()
        .default_reasoning_effort()
        .map(|effort| effort.as_str().to_owned());
    let (default_reasoning, reasoning_efforts) = match reasoning_evidence {
        GrokCatalogCapabilityEvidence::DeclaredNative if !catalog_reasoning_efforts.is_empty() => {
            let default_reasoning = catalog_default_reasoning
                .filter(|default| {
                    catalog_reasoning_efforts
                        .iter()
                        .any(|effort| effort == default)
                })
                .or_else(|| catalog_reasoning_efforts.first().cloned());
            (default_reasoning, catalog_reasoning_efforts)
        }
        GrokCatalogCapabilityEvidence::DeclaredNative => (None, Vec::new()),
        GrokCatalogCapabilityEvidence::DeclaredUnsupported => {
            (Some("none".to_owned()), vec!["none".to_owned()])
        }
        GrokCatalogCapabilityEvidence::Unknown => (None, Vec::new()),
    };
    let context_window_tokens = model
        .limits()
        .context_window_tokens()
        .map(std::num::NonZeroU64::get)
        .or(known_grok_4_5.then_some(500_000));
    let tool_evidence = model.capabilities().streaming_tool_calls();

    ModelPresentation::new(
        model
            .display_name()
            .map(str::to_owned)
            .or_else(|| known_grok_4_5.then(|| "Grok 4.5".to_owned())),
        model
            .metadata()
            .description()
            .map(str::to_owned)
            .or_else(|| {
                known_grok_4_5
                    .then(|| "xAI Grok 4.5 frontier model with reasoning and vision.".to_owned())
            }),
    )
    .with_reasoning(default_reasoning, reasoning_efforts)
    .with_context_window_tokens(context_window_tokens)
    .with_image_input(known_grok_4_5)
    .with_agent_tools(
        tool_evidence != GrokCatalogCapabilityEvidence::DeclaredUnsupported,
        tool_evidence == GrokCatalogCapabilityEvidence::DeclaredNative
            || (known_grok_4_5 && tool_evidence == GrokCatalogCapabilityEvidence::Unknown),
    )
    .with_search_tool(
        model.capabilities().backend_search() == GrokCatalogCapabilityEvidence::DeclaredNative,
    )
    .with_hidden(model.metadata().hidden().unwrap_or(false))
}

fn is_known_grok_4_5_model(slug: &str) -> bool {
    matches!(
        slug,
        DEFAULT_GROK_MODEL | "grok-4.5-latest" | "grok-4.5-build-free" | "grok-build-latest"
    )
}

#[derive(Clone, Serialize, Deserialize)]
struct XaiSessionState {
    account_id: String,
    session_id: Option<String>,
    transcript: Vec<XaiReplayItem>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum XaiReplayItem {
    ClientInput(Value),
    SanitizedOutput(Value),
    AccountOutput { account_id: String, item: Value },
}

struct GrokSessionCapture {
    previous: Option<XaiSessionState>,
    request_input: Vec<Value>,
    account_id: String,
    session_id: Option<String>,
    output_items: BTreeMap<u32, Value>,
}

fn decode_xai_session_state(
    request: &GenerateRequest,
) -> Result<Option<XaiSessionState>, ProviderError> {
    request
        .provider_session_state(XAI_PROVIDER_NAME)
        .map(|state| {
            let payload = Value::Object(state.payload().clone());
            if serde_json::to_vec(&payload)
                .map_err(|_| protocol_not_sent())?
                .len()
                > XAI_SESSION_STATE_MAX_BYTES
            {
                return Err(protocol_not_sent());
            }
            serde_json::from_value(payload).map_err(|_| protocol_not_sent())
        })
        .transpose()
}

fn encode_xai_session_state(
    state: XaiSessionState,
) -> Result<Option<ProviderSessionState>, ProviderError> {
    let value = serde_json::to_value(state).map_err(|_| protocol_sent())?;
    if serde_json::to_vec(&value)
        .map_err(|_| protocol_sent())?
        .len()
        > XAI_SESSION_STATE_MAX_BYTES
    {
        return Ok(None);
    }
    let Value::Object(payload) = value else {
        return Err(protocol_sent());
    };
    ProviderSessionState::new(XAI_PROVIDER_NAME, payload)
        .map(Some)
        .map_err(|_| protocol_sent())
}

fn continuation_account(
    context: &AttemptContext,
    previous_session: Option<&XaiSessionState>,
) -> Result<Option<gateway_core::engine::credential::ProviderAccountId>, ProviderError> {
    let Some(continuation) = context.continuation() else {
        return Ok(None);
    };
    match (context.continuation_attempt(), continuation) {
        (ContinuationAttempt::Native, ContinuationBinding::Pinned(pin)) => {
            if pin.provider().as_str() != XAI_PROVIDER_NAME {
                return Err(invalid_continuation());
            }
            Ok(Some(pin.account().clone()))
        }
        (ContinuationAttempt::Native, ContinuationBinding::External(_)) => {
            Err(invalid_continuation())
        }
        (ContinuationAttempt::ReplayOwner, _) => {
            let previous = previous_session.ok_or_else(invalid_continuation)?;
            gateway_core::engine::credential::ProviderAccountId::new(previous.account_id.clone())
                .map(Some)
                .map_err(|_| invalid_continuation())
        }
        (ContinuationAttempt::ReplayAny, _) => Ok(None),
        (ContinuationAttempt::None, _) => Err(invalid_continuation()),
    }
}

fn apply_continuation(
    request: &mut GrokResponsesRequest,
    previous_session: Option<&XaiSessionState>,
    context: &AttemptContext,
    account: &gateway_core::engine::credential::ProviderAccountId,
    current_input: &[Value],
) -> Result<(), ProviderError> {
    let Some(continuation) = context.continuation() else {
        return Ok(());
    };
    match context.continuation_attempt() {
        ContinuationAttempt::Native => {
            let ContinuationBinding::Pinned(pin) = continuation else {
                return Err(invalid_continuation());
            };
            let provider = ProviderKind::new(XAI_PROVIDER_NAME).map_err(|_| protocol_not_sent())?;
            if !pin.matches(&provider, account) {
                return Err(invalid_continuation());
            }
            request.set_previous_response_id(Some(pin.upstream_response_id().as_str().to_owned()));
            Ok(())
        }
        ContinuationAttempt::ReplayOwner | ContinuationAttempt::ReplayAny => {
            let previous = previous_session.ok_or_else(invalid_continuation)?;
            if context.continuation_attempt() == ContinuationAttempt::ReplayOwner
                && previous.account_id != account.as_str()
            {
                return Err(invalid_continuation());
            }
            let mut input = replay_input_for_account(previous, account.as_str(), true);
            input.reserve(current_input.len());
            input.extend(current_input.iter().cloned());
            request.set_replay_input(input).map_err(map_request_error)?;
            request.set_previous_response_id(None);
            request.inherit_session(None);
            Ok(())
        }
        ContinuationAttempt::None => Err(invalid_continuation()),
    }
}

fn replay_input_for_account(
    state: &XaiSessionState,
    account_id: &str,
    force_portable: bool,
) -> Vec<Value> {
    state
        .transcript
        .iter()
        .filter_map(|item| match item {
            XaiReplayItem::ClientInput(value) | XaiReplayItem::SanitizedOutput(value) => {
                Some(value.clone())
            }
            XaiReplayItem::AccountOutput {
                account_id: owner,
                item,
            } if owner == account_id && !force_portable => {
                portable_output_item(item.clone(), false)
            }
            XaiReplayItem::AccountOutput { item, .. } => portable_output_item(item.clone(), true),
        })
        .collect()
}

fn project_transcript_to_account(transcript: &mut Vec<XaiReplayItem>, account_id: &str) {
    *transcript = transcript
        .drain(..)
        .filter_map(|item| match item {
            XaiReplayItem::AccountOutput {
                account_id: owner,
                item,
            } if owner != account_id => {
                portable_output_item(item, true).map(XaiReplayItem::SanitizedOutput)
            }
            item => Some(item),
        })
        .collect();
}

fn portable_output_item(mut item: Value, strip_opaque: bool) -> Option<Value> {
    let Value::Object(object) = &mut item else {
        return None;
    };
    let is_reasoning = object.get("type").and_then(Value::as_str) == Some("reasoning");
    if !matches!(
        object.get("type").and_then(Value::as_str),
        Some("reasoning" | "message" | "function_call" | "custom_tool_call")
    ) {
        return None;
    }
    object.remove("id");
    object.remove("status");
    if is_reasoning {
        if strip_opaque
            || object
                .get("encrypted_content")
                .and_then(Value::as_str)
                .is_some_and(|value| !valid_reasoning_ciphertext(value))
        {
            object.remove("encrypted_content");
        }
        if object.get("encrypted_content").is_none() && !has_readable_reasoning(object) {
            return None;
        }
    }
    Some(item)
}

fn has_readable_reasoning(item: &Map<String, Value>) -> bool {
    ["summary", "content"].into_iter().any(|field| {
        item.get(field)
            .and_then(Value::as_array)
            .is_some_and(|parts| {
                parts.iter().any(|part| {
                    part.get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.trim().is_empty())
                })
            })
    })
}

fn attach_xai_session_update(
    events: &mut [ProviderEvent],
    capture: &mut Option<GrokSessionCapture>,
) -> Result<(), ProviderError> {
    if capture.is_none() {
        return Ok(());
    }
    let mut terminal_index = None;
    for (index, event) in events.iter().enumerate() {
        if let Some(capture) = capture.as_mut() {
            capture_output_item(event, capture);
        }
        if event
            .canonical_facts()
            .iter()
            .any(|fact| matches!(fact, GatewayEvent::Completed(_)))
        {
            terminal_index = Some(index);
        }
    }
    let Some(terminal_index) = terminal_index else {
        return Ok(());
    };
    let Some(mut capture) = capture.take() else {
        return Ok(());
    };
    let mut transcript = capture
        .previous
        .take()
        .map(|state| state.transcript)
        .unwrap_or_default();
    project_transcript_to_account(&mut transcript, &capture.account_id);
    transcript.extend(
        capture
            .request_input
            .into_iter()
            .map(XaiReplayItem::ClientInput),
    );
    transcript.extend(capture.output_items.into_values().map(|item| {
        XaiReplayItem::AccountOutput {
            account_id: capture.account_id.clone(),
            item,
        }
    }));
    let state = XaiSessionState {
        account_id: capture.account_id,
        session_id: capture.session_id,
        transcript,
    };
    if let Some(update) = encode_xai_session_state(state)? {
        events[terminal_index].attach_session_update(update);
    }
    Ok(())
}

fn capture_output_item(event: &ProviderEvent, capture: &mut GrokSessionCapture) {
    let Some(wire) = event.wire_event() else {
        return;
    };
    let event_type = wire
        .event_type()
        .or_else(|| wire.data().get("type").and_then(Value::as_str));
    if event_type == Some("response.output_item.done")
        && capture.output_items.len() < XAI_SESSION_OUTPUT_LIMIT
        && let Some(item) = wire
            .data()
            .get("item")
            .cloned()
            .and_then(|item| portable_output_item(item, false))
    {
        let index = wire
            .data()
            .get("output_index")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or_else(|| u32::try_from(capture.output_items.len()).unwrap_or(u32::MAX));
        capture.output_items.insert(index, item);
    }
    if matches!(
        event_type,
        Some("response.completed" | "response.incomplete")
    ) && let Some(output) = wire
        .data()
        .get("response")
        .and_then(Value::as_object)
        .and_then(|response| response.get("output"))
        .and_then(Value::as_array)
        .filter(|output| !output.is_empty())
    {
        for (index, item) in output.iter().take(XAI_SESSION_OUTPUT_LIMIT).enumerate() {
            let Some(index) = u32::try_from(index).ok() else {
                break;
            };
            if capture.output_items.contains_key(&index) {
                continue;
            }
            if let Some(item) = portable_output_item(item.clone(), false) {
                capture.output_items.insert(index, item);
            }
        }
    }
}

fn invalid_continuation() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidRequest,
        UpstreamSendState::NotSent,
    )
    .with_continuation_failure(ContinuationFailure::HistoryUnavailable)
}

fn protocol_not_sent() -> ProviderError {
    provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
}

fn protocol_sent() -> ProviderError {
    provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
}

struct GrokStreamAttempt {
    client_identity: GrokClientIdentity,
    wire_profile: XaiWireProfileState,
    credential_recovery: Arc<dyn GrokCredentialRecovery>,
    responses_url: Url,
    request: GrokResponsesRequest,
    upstream_model: UpstreamModelId,
    context: AttemptContext,
    session: Arc<SelectedGrokSession>,
    output_started_at: Instant,
    session_capture: Option<GrokSessionCapture>,
    reasoning_replay_capture: Option<GrokReasoningReplayCapture>,
}

struct GrokCompactionStreamAttempt {
    client_identity: GrokClientIdentity,
    wire_profile: XaiWireProfileState,
    credential_recovery: Arc<dyn GrokCredentialRecovery>,
    responses_url: Url,
    request: GrokCompactionRequest,
    upstream_model: UpstreamModelId,
    upstream_session_id: Option<String>,
    context: AttemptContext,
    session: Arc<SelectedGrokSession>,
    reasoning_replay: GrokReasoningReplay,
    reasoning_replay_key: Option<GrokReasoningReplayKey>,
}

struct AcceptedGrokInference {
    response: GrokInferenceResponse,
    observation: ProviderResponseObservation,
}

struct GrokInferenceStartFailure {
    observation: Option<ProviderResponseObservation>,
    error: ProviderError,
}

async fn start_grok_inference(
    selector: &dyn GrokSessionSelector,
    transport: &dyn GrokInferenceTransport,
    credential_recovery: &dyn GrokCredentialRecovery,
    request: GrokInferenceRequest,
    upstream_model: &UpstreamModelId,
    context: &AttemptContext,
    session: &SelectedGrokSession,
) -> Result<AcceptedGrokInference, GrokInferenceStartFailure> {
    if context.cancellation().is_cancelled() {
        return Err(GrokInferenceStartFailure {
            observation: None,
            error: provider_error(ProviderErrorKind::Cancelled, UpstreamSendState::NotSent),
        });
    }
    let Some(handshake_deadline) = remaining(context.deadline()) else {
        return Err(GrokInferenceStartFailure {
            observation: None,
            error: provider_error(ProviderErrorKind::Timeout, UpstreamSendState::NotSent),
        });
    };
    let cancellation = context.cancellation().clone();
    let boundary = tokio::select! {
        biased;
        _ = cancellation.cancelled() => InferenceBoundary::Cancelled,
        _ = tokio::time::sleep(handshake_deadline) => InferenceBoundary::Deadline,
        response = transport.execute(request) => InferenceBoundary::Response(response),
    };
    let response = match boundary {
        InferenceBoundary::Cancelled => {
            return Err(GrokInferenceStartFailure {
                observation: None,
                error: provider_error(ProviderErrorKind::Cancelled, UpstreamSendState::Ambiguous),
            });
        }
        InferenceBoundary::Deadline => {
            return Err(GrokInferenceStartFailure {
                observation: None,
                error: provider_error(ProviderErrorKind::Timeout, UpstreamSendState::Ambiguous),
            });
        }
        InferenceBoundary::Response(Ok(response)) => response,
        InferenceBoundary::Response(Err(error)) => {
            let observation = xai_error_observation(&error).ok();
            let credential_failure = transport_credential_failure(&error, upstream_model);
            let error =
                map_continuation_failure(context, map_transport_error_for_context(error, context));
            let error = recover_or_record_failure(
                selector,
                credential_recovery,
                session,
                error,
                credential_failure,
                context.credential_recovery_attempted(),
            )
            .await;
            return Err(GrokInferenceStartFailure { observation, error });
        }
    };
    let observation =
        xai_response_observation(&response).map_err(|error| GrokInferenceStartFailure {
            observation: None,
            error,
        })?;
    Ok(AcceptedGrokInference {
        response,
        observation,
    })
}

async fn next_grok_chunk(
    body: &mut GrokInferenceChunkStream,
    selector: &dyn GrokSessionSelector,
    session: &SelectedGrokSession,
    upstream_model: &UpstreamModelId,
    context: &AttemptContext,
) -> Result<Option<bytes::Bytes>, ProviderError> {
    let Some(stream_deadline) = remaining(context.deadline()) else {
        return Err(provider_error(
            ProviderErrorKind::Timeout,
            UpstreamSendState::Sent,
        ));
    };
    let cancellation = context.cancellation().clone();
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(provider_error(
            ProviderErrorKind::Cancelled,
            UpstreamSendState::Sent,
        )),
        _ = tokio::time::sleep(stream_deadline) => Err(provider_error(
            ProviderErrorKind::Timeout,
            UpstreamSendState::Sent,
        )),
        chunk = body.next() => match chunk {
            Some(Ok(chunk)) => Ok(Some(chunk)),
            Some(Err(error)) => Err(map_and_record_stream_transport_failure(
                selector,
                session,
                error,
                upstream_model,
            ).await),
            None => Ok(None),
        },
    }
}

fn cold_compaction_http_sse_stream(
    selector: Arc<dyn GrokSessionSelector>,
    transport: Arc<dyn GrokInferenceTransport>,
    attempt: GrokCompactionStreamAttempt,
) -> EventStream {
    let GrokCompactionStreamAttempt {
        client_identity,
        wire_profile,
        credential_recovery,
        responses_url,
        request,
        upstream_model,
        upstream_session_id,
        context,
        session,
        reasoning_replay,
        reasoning_replay_key,
    } = attempt;
    Box::pin(async_stream::try_stream! {
        let headers = build_grok_headers(
            &wire_profile,
            &session,
            &client_identity,
            context.request_id(),
            upstream_session_id.as_deref(),
            None,
            &upstream_model,
        );
        let body = request.to_json_bytes().map_err(map_request_error)?;
        let inference_request = GrokInferenceRequest::new(
            responses_url,
            headers,
            body,
            session.binding().clone(),
        );
        let accepted = match start_grok_inference(
            selector.as_ref(),
            transport.as_ref(),
            credential_recovery.as_ref(),
            inference_request,
            &upstream_model,
            &context,
            &session,
        )
        .await
        {
            Ok(accepted) => accepted,
            Err(failure) => {
                if let Some(observation) = failure.observation {
                    yield ProviderEvent::observation(observation);
                }
                Err(mark_transient_compaction_failure(failure.error))?;
                return;
            }
        };
        yield ProviderEvent::observation(accepted.observation);

        let mut body = accepted.response.into_body();
        let mut canonical = GrokCanonicalDecoder::new(upstream_model.as_str());
        let mut summary = GrokCompactionSummaryDecoder::new();
        let mut facts = CompactionFacts::default();

        'stream: while let Some(chunk) = next_grok_chunk(
            &mut body,
            selector.as_ref(),
            &session,
            &upstream_model,
            &context,
        )
        .await
        .map_err(mark_transient_compaction_failure)?
        {
            let events = match canonical.push(&chunk) {
                Ok(events) => events,
                Err(error) => {
                    let error = map_continuation_failure(&context, error);
                    let error = record_stream_failure(
                        selector.as_ref(),
                        &session,
                        error,
                        &upstream_model,
                    )
                    .await;
                    Err(mark_transient_compaction_failure(error))?;
                    return;
                }
            };
            for event in events {
                summary.observe(&event).map_err(map_compaction_decode_error)?;
                facts.observe(&event);
                if facts.completed.is_some() {
                    break 'stream;
                }
            }
        }

        if facts.completed.is_none() {
            let events = match canonical.finish_without_terminal() {
                Ok(events) => events,
                Err(error) => {
                    let error = map_continuation_failure(&context, error);
                    let error = record_stream_failure(
                        selector.as_ref(),
                        &session,
                        error,
                        &upstream_model,
                    )
                    .await;
                    Err(mark_transient_compaction_failure(error))?;
                    return;
                }
            };
            for event in events {
                summary.observe(&event).map_err(map_compaction_decode_error)?;
                facts.observe(&event);
                if facts.completed.is_some() {
                    break;
                }
            }
        }

        let summary = summary.finish().map_err(map_compaction_decode_error)?;
        let started = facts.started.ok_or_else(protocol_sent)?;
        let upstream_completed = facts.completed.is_some();
        let completed = facts.completed.unwrap_or_else(|| started.clone());
        let (created, output_done, terminal) = crate::transport::compaction::compaction_wire_events(
            &started,
            &completed,
            &summary,
            facts.created_response.as_ref(),
            facts.terminal_response.as_ref(),
            facts.terminal_was_incomplete,
        )
        .map_err(|_| protocol_sent())?
        .into_parts();
        ensure_sent_context(&context)?;
        if upstream_completed && session.allows_account_state_mutation() {
            selector.record_success(&session).await;
        }
        if let Some(key) = reasoning_replay_key.as_ref() {
            reasoning_replay.clear(key);
        }
        yield ProviderEvent::canonical_with_wire(vec![GatewayEvent::Started(started)], created);
        yield ProviderEvent::wire(output_done);
        let mut terminal_facts = facts.accounting;
        terminal_facts.push(GatewayEvent::Completed(completed));
        yield ProviderEvent::canonical_with_wire(terminal_facts, terminal);
    })
}

#[derive(Default)]
struct CompactionFacts {
    started: Option<ResponseMeta>,
    completed: Option<ResponseMeta>,
    accounting: Vec<GatewayEvent>,
    created_response: Option<Value>,
    terminal_response: Option<Value>,
    terminal_was_incomplete: bool,
}

impl CompactionFacts {
    fn observe(&mut self, event: &ProviderEvent) {
        self.capture_wire_response(event);
        for fact in event.canonical_facts() {
            match fact {
                GatewayEvent::Started(meta) if self.started.is_none() => {
                    self.started = Some(meta.clone());
                }
                GatewayEvent::Completed(meta) if self.completed.is_none() => {
                    self.completed = Some(meta.clone());
                }
                GatewayEvent::Usage(_)
                | GatewayEvent::CalculatedCost(_)
                | GatewayEvent::ProviderCost(_) => self.accounting.push(fact.clone()),
                _ => {}
            }
        }
    }

    fn capture_wire_response(&mut self, event: &ProviderEvent) {
        let Some(wire) = event.wire_event().filter(|wire| wire.has_json_data()) else {
            return;
        };
        let event_type = wire
            .event_type()
            .or_else(|| wire.data().get("type").and_then(Value::as_str));
        let response = wire.data().get("response").cloned();
        match event_type {
            Some("response.created" | "response.in_progress")
                if self.created_response.is_none() =>
            {
                self.created_response = response;
            }
            Some("response.completed" | "response.incomplete")
                if self.terminal_response.is_none() =>
            {
                self.terminal_was_incomplete = event_type == Some("response.incomplete");
                self.terminal_response = response;
            }
            _ => {}
        }
    }
}

fn map_compaction_decode_error(error: GrokCompactionDecodeError) -> ProviderError {
    match error {
        GrokCompactionDecodeError::Degenerate => mark_transient_compaction_failure(protocol_sent()),
    }
}

fn mark_transient_compaction_failure(error: ProviderError) -> ProviderError {
    if matches!(
        error.kind(),
        ProviderErrorKind::RateLimited
            | ProviderErrorKind::Timeout
            | ProviderErrorKind::Transport
            | ProviderErrorKind::Protocol
            | ProviderErrorKind::Unavailable
    ) {
        error.with_pre_delivery_retry()
    } else {
        error
    }
}

fn cold_http_sse_stream(
    selector: Arc<dyn GrokSessionSelector>,
    transport: Arc<dyn GrokInferenceTransport>,
    attempt: GrokStreamAttempt,
) -> EventStream {
    let GrokStreamAttempt {
        client_identity,
        wire_profile,
        credential_recovery,
        responses_url,
        request,
        upstream_model,
        context,
        session,
        output_started_at,
        mut session_capture,
        mut reasoning_replay_capture,
    } = attempt;
    Box::pin(async_stream::try_stream! {
        if context.cancellation().is_cancelled() {
            Err(provider_error(
                ProviderErrorKind::Cancelled,
                UpstreamSendState::NotSent,
            ))?;
        }
        let headers = build_grok_headers(
            &wire_profile,
            &session,
            &client_identity,
            context.request_id(),
            request.session_id(),
            None,
            &upstream_model,
        );
        let body = request.to_json_bytes().map_err(map_request_error)?;
        let inference_request = GrokInferenceRequest::new(
            responses_url,
            headers,
            body,
            session.binding().clone(),
        );
        let Some(handshake_deadline) = remaining(context.deadline()) else {
            Err(provider_error(
                ProviderErrorKind::Timeout,
                UpstreamSendState::NotSent,
            ))?;
            return;
        };
        let cancellation = context.cancellation().clone();
        let boundary = tokio::select! {
            biased;
            _ = cancellation.cancelled() => InferenceBoundary::Cancelled,
            _ = tokio::time::sleep(handshake_deadline) => InferenceBoundary::Deadline,
            response = transport.execute(inference_request) => InferenceBoundary::Response(response),
        };
        let response = match boundary {
            InferenceBoundary::Cancelled => {
                Err(provider_error(ProviderErrorKind::Cancelled, UpstreamSendState::Ambiguous))?;
                return;
            }
            InferenceBoundary::Deadline => {
                Err(provider_error(ProviderErrorKind::Timeout, UpstreamSendState::Ambiguous))?;
                return;
            }
            InferenceBoundary::Response(Ok(response)) => response,
            InferenceBoundary::Response(Err(error)) => {
                let observation = xai_error_observation(&error)?;
                let credential_failure = transport_credential_failure(&error, &upstream_model);
                let error =
                    map_continuation_failure(&context, map_transport_error_for_context(error, &context));
                let error = recover_or_record_failure(
                    selector.as_ref(),
                    credential_recovery.as_ref(),
                    &session,
                    error,
                    credential_failure,
                    context.credential_recovery_attempted(),
                )
                .await;
                yield ProviderEvent::observation(observation);
                Err(error)?;
                return;
            }
        };

        let observation = xai_response_observation(&response)?;
        let base_timings = observation.timings();
        let mut first_token_ms: Option<u64> = None;
        yield ProviderEvent::observation(observation.clone());

        let mut body = response.into_body();
        let mut decoder = GrokCanonicalDecoder::for_request(upstream_model.as_str(), &request);
        loop {
            let Some(stream_deadline) = remaining(context.deadline()) else {
                Err(provider_error(
                    ProviderErrorKind::Timeout,
                    UpstreamSendState::Sent,
                ))?;
                return;
            };
            let next = tokio::select! {
                biased;
                _ = cancellation.cancelled() => Err(provider_error(
                    ProviderErrorKind::Cancelled,
                    UpstreamSendState::Sent,
                )),
                _ = tokio::time::sleep(stream_deadline) => Err(provider_error(
                    ProviderErrorKind::Timeout,
                    UpstreamSendState::Sent,
                )),
                chunk = body.next() => match chunk {
                    Some(Ok(chunk)) => Ok(Some(chunk)),
                    Some(Err(error)) => Err(map_and_record_stream_transport_failure(
                        selector.as_ref(),
                        &session,
                        error,
                        &upstream_model,
                    ).await),
                    None => Ok(None),
                },
            }?;
            let Some(chunk) = next else {
                break;
            };
            let mut events = match decoder.push(&chunk) {
                Ok(events) => events,
                Err(error) => {
                    let error = map_continuation_failure(&context, error);
                    let error = record_stream_failure(
                        selector.as_ref(),
                        &session,
                        error,
                        &upstream_model,
                    )
                    .await;
                    Err(error)?;
                    return;
                }
            };
            // 首个非前导输出事件（结构帧也算）即上报携带 first_token_ms 的观测，
            // 供 Core 覆盖会话级兜底值。
            if decoder.take_output_start() && first_token_ms.is_none() {
                first_token_ms = Some(
                    u64::try_from(output_started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                );
                yield ProviderEvent::observation(
                    observation
                        .clone()
                        .with_timings(ProviderResponseTimings {
                            first_token_ms,
                            ..base_timings
                        }),
                );
            }
            let completed = events
                .iter()
                .flat_map(ProviderEvent::canonical_facts)
                .any(|event| matches!(event, GatewayEvent::Completed(_)));
            attach_xai_session_update(&mut events, &mut session_capture)?;
            if let Some(capture) = reasoning_replay_capture.as_mut() {
                capture.observe(&events);
            }
            if completed && session.allows_account_state_mutation() {
                selector.record_success(&session).await;
            }
            for event in events {
                ensure_sent_context(&context)?;
                yield event;
            }
            if completed {
                return;
            }
        }
        let mut final_events = match decoder.finish() {
            Ok(events) => events,
            Err(error) => {
                let error = map_continuation_failure(&context, error);
                let error = record_stream_failure(
                    selector.as_ref(),
                    &session,
                    error,
                    &upstream_model,
                )
                .await;
                Err(error)?;
                return;
            }
        };
        let completed = final_events
            .iter()
            .flat_map(ProviderEvent::canonical_facts)
            .any(|event| matches!(event, GatewayEvent::Completed(_)));
        // 尾部 finish 补全缓冲中的首个输出帧时，同样上报首字。
        if decoder.take_output_start() && first_token_ms.is_none() {
            first_token_ms = Some(
                u64::try_from(output_started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            );
            yield ProviderEvent::observation(
                observation
                    .clone()
                    .with_timings(ProviderResponseTimings {
                        first_token_ms,
                        ..base_timings
                    }),
            );
        }
        attach_xai_session_update(&mut final_events, &mut session_capture)?;
        if let Some(capture) = reasoning_replay_capture.as_mut() {
            capture.observe(&final_events);
        }
        if completed && session.allows_account_state_mutation() {
            selector.record_success(&session).await;
        }
        for event in final_events {
            ensure_sent_context(&context)?;
            yield event;
        }
    })
}

enum InferenceBoundary {
    Response(Result<crate::transport::GrokInferenceResponse, GrokInferenceTransportError>),
    Cancelled,
    Deadline,
}

fn xai_error_observation(
    error: &GrokInferenceTransportError,
) -> Result<ProviderResponseObservation, ProviderError> {
    let mut observation = ProviderResponseObservation::new(
        UpstreamTransport::new(HTTP_SSE_TRANSPORT)
            .map_err(|_| provider_error(ProviderErrorKind::Protocol, error.send_state()))?,
    );
    if let Some(http_version) = error.http_version() {
        observation = observation.with_http_version(http_version);
    }
    if let Some(status_code) = error.status() {
        observation = observation.with_status_code(status_code);
    }
    if let Some(request_id) = error.request_id().cloned() {
        observation = observation.with_request_id(request_id);
    }
    Ok(with_xai_transport_metrics(
        observation,
        error.transport_metrics(),
    ))
}

fn xai_response_observation(
    response: &GrokInferenceResponse,
) -> Result<ProviderResponseObservation, ProviderError> {
    let mut observation = ProviderResponseObservation::new(
        UpstreamTransport::new(HTTP_SSE_TRANSPORT)
            .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent))?,
    )
    .with_http_version(response.http_version())
    .with_status_code(response.status_code());
    if let Some(request_id) = response.request_id().cloned() {
        observation = observation.with_request_id(request_id);
    }
    Ok(with_xai_transport_metrics(
        observation,
        response.transport_metrics(),
    ))
}

fn with_xai_transport_metrics(
    mut observation: ProviderResponseObservation,
    metrics: GrokInferenceTransportMetrics,
) -> ProviderResponseObservation {
    observation = observation.with_timings(ProviderResponseTimings {
        headers_ms: metrics.headers_ms(),
        ..ProviderResponseTimings::default()
    });
    if let Some(metadata) = xai_transport_metadata(metrics) {
        observation = observation.with_provider_metadata(metadata);
    }
    observation
}

fn xai_transport_metadata(
    metrics: GrokInferenceTransportMetrics,
) -> Option<ProviderResponseMetadata> {
    let mut metadata = Map::new();
    if let Some(status) = metrics.client_cache_status() {
        metadata.insert(
            "clientCache".to_owned(),
            Value::String(status.as_str().to_owned()),
        );
    }
    if let Some(dns) = metrics.dns() {
        metadata.insert(
            "dnsSource".to_owned(),
            Value::String(dns.source().as_str().to_owned()),
        );
        metadata.insert("dnsMs".to_owned(), Value::from(dns.duration_ms()));
    }
    if let Some(headers_ms) = metrics.headers_ms() {
        metadata.insert("upstreamHeadersMs".to_owned(), Value::from(headers_ms));
    }
    if metadata.is_empty() {
        return None;
    }
    // 观测对象版本信封：读取端按 schemaVersion 解码，缺失视为 v0。
    metadata.insert("schemaVersion".to_owned(), Value::from(1));
    ProviderResponseMetadata::new(serde_json::to_string(&Value::Object(metadata)).ok()?)
}

async fn record_credential_failure(
    selector: &dyn GrokSessionSelector,
    session: &SelectedGrokSession,
    error: ProviderError,
    failure: GrokCredentialFailure,
) -> ProviderError {
    if session.allows_account_state_mutation() {
        selector.record_failure(session, failure).await;
    }
    error
}

async fn record_stream_failure(
    selector: &dyn GrokSessionSelector,
    session: &SelectedGrokSession,
    error: ProviderError,
    upstream_model: &UpstreamModelId,
) -> ProviderError {
    if !session.allows_account_state_mutation() {
        return error;
    }
    if let Some(failure) = stream_credential_failure(&error, upstream_model) {
        return record_credential_failure(selector, session, error, failure).await;
    }
    if error.kind() != ProviderErrorKind::Cancelled {
        selector
            .record_failure(session, GrokCredentialFailure::StreamInterrupted)
            .await;
    }
    error
}

fn stream_credential_failure(
    error: &ProviderError,
    upstream_model: &UpstreamModelId,
) -> Option<GrokCredentialFailure> {
    match error.kind() {
        ProviderErrorKind::Unauthorized => Some(GrokCredentialFailure::Unauthorized),
        ProviderErrorKind::RateLimited => Some(GrokCredentialFailure::RateLimited {
            retry_after: error.retry_after(),
        }),
        ProviderErrorKind::QuotaExhausted => {
            let visible = error.client_visible_upstream_error();
            let quota_kind = classify_grok_quota_failure(
                visible
                    .and_then(ClientVisibleUpstreamError::code)
                    .or_else(|| error.upstream_code().map(|code| code.as_str())),
                visible.and_then(ClientVisibleUpstreamError::error_type),
                visible.map(ClientVisibleUpstreamError::message),
            );
            Some(match quota_kind {
                Some(GrokQuotaFailureKind::FreeAccount) => {
                    GrokCredentialFailure::FreeQuotaExhausted
                }
                Some(GrokQuotaFailureKind::FreeModelUsage) => {
                    GrokCredentialFailure::ModelQuotaExhausted {
                        upstream_model: upstream_model.clone(),
                        retry_after: error.retry_after(),
                    }
                }
                Some(GrokQuotaFailureKind::Account) | None => GrokCredentialFailure::QuotaExhausted,
            })
        }
        _ => None,
    }
}

async fn recover_or_record_failure(
    selector: &dyn GrokSessionSelector,
    recovery: &dyn GrokCredentialRecovery,
    session: &SelectedGrokSession,
    error: ProviderError,
    credential_failure: Option<GrokCredentialFailure>,
    recovery_attempted: bool,
) -> ProviderError {
    if !session.allows_account_state_mutation() {
        return error;
    }
    if error.requires_credential_recovery() && !recovery_attempted {
        return match recovery
            .recover_unauthorized(session.account_id(), session.credential_revision())
            .await
        {
            GrokCredentialRecoveryOutcome::Recovered => error.with_same_account_retry(),
            GrokCredentialRecoveryOutcome::Rejected => error,
            GrokCredentialRecoveryOutcome::Unavailable => match credential_failure {
                Some(failure) => record_credential_failure(selector, session, error, failure).await,
                None => error,
            },
        };
    }
    match credential_failure {
        Some(failure) => record_credential_failure(selector, session, error, failure).await,
        None => error,
    }
}

fn transport_credential_failure(
    error: &GrokInferenceTransportError,
    upstream_model: &UpstreamModelId,
) -> Option<GrokCredentialFailure> {
    match error.kind() {
        GrokInferenceTransportErrorKind::Unauthorized => Some(GrokCredentialFailure::Unauthorized),
        GrokInferenceTransportErrorKind::RateLimited => Some(GrokCredentialFailure::RateLimited {
            retry_after: error.retry_after(),
        }),
        GrokInferenceTransportErrorKind::QuotaExhausted => {
            Some(GrokCredentialFailure::QuotaExhausted)
        }
        GrokInferenceTransportErrorKind::FreeQuotaExhausted => {
            Some(GrokCredentialFailure::FreeQuotaExhausted)
        }
        GrokInferenceTransportErrorKind::PaymentRequired => {
            Some(GrokCredentialFailure::PaymentRequired {
                retry_after: error.retry_after(),
            })
        }
        GrokInferenceTransportErrorKind::ModelQuotaExhausted => {
            Some(GrokCredentialFailure::ModelQuotaExhausted {
                upstream_model: upstream_model.clone(),
                retry_after: error.retry_after(),
            })
        }
        GrokInferenceTransportErrorKind::ModelAccessDenied => {
            Some(GrokCredentialFailure::ModelAccessDenied {
                upstream_model: upstream_model.clone(),
                retry_after: error.retry_after(),
            })
        }
        _ => None,
    }
}

async fn map_and_record_stream_transport_failure(
    selector: &dyn GrokSessionSelector,
    session: &SelectedGrokSession,
    error: GrokInferenceTransportError,
    upstream_model: &UpstreamModelId,
) -> ProviderError {
    let request_scoped = error.kind() == GrokInferenceTransportErrorKind::SafetyRejected;
    let credential_failure = transport_credential_failure(&error, upstream_model);
    let error = map_stream_error(error);
    match credential_failure {
        Some(failure) => record_credential_failure(selector, session, error, failure).await,
        None if request_scoped => error,
        None => record_stream_failure(selector, session, error, upstream_model).await,
    }
}

fn map_continuation_failure(context: &AttemptContext, error: ProviderError) -> ProviderError {
    let is_reasoning_decode_failure = context.continuation_attempt() == ContinuationAttempt::Native
        && error.kind() == ProviderErrorKind::InvalidRequest
        && error.upstream_status() == Some(400)
        && error
            .upstream_code()
            .is_some_and(|code| code.as_str() == REASONING_DECODE_FAILED_CODE);
    let is_missing_native_response = context.continuation_attempt() == ContinuationAttempt::Native
        && error.kind() == ProviderErrorKind::InvalidRequest
        && error.upstream_status() == Some(404)
        && error
            .upstream_code()
            .is_some_and(|code| code.as_str() == RESPONSE_NOT_FOUND_CODE);
    if is_reasoning_decode_failure || is_missing_native_response {
        error
            .with_continuation_failure(ContinuationFailure::HistoryUnavailable)
            .with_replay_safe()
    } else {
        error
    }
}

fn preflight_context(context: &AttemptContext) -> Result<(), ProviderError> {
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
    Ok(())
}

fn ensure_sent_context(context: &AttemptContext) -> Result<(), ProviderError> {
    if context.cancellation().is_cancelled() {
        return Err(provider_error(
            ProviderErrorKind::Cancelled,
            UpstreamSendState::Sent,
        ));
    }
    if remaining(context.deadline()).is_none() {
        return Err(provider_error(
            ProviderErrorKind::Timeout,
            UpstreamSendState::Sent,
        ));
    }
    Ok(())
}

fn map_request_error(error: GrokRequestEncodeError) -> ProviderError {
    let kind = match error {
        GrokRequestEncodeError::InvalidProtocolPayload
        | GrokRequestEncodeError::InvalidRequestNormalization
        | GrokRequestEncodeError::InvalidRequestField { .. } => ProviderErrorKind::InvalidRequest,
        GrokRequestEncodeError::Serialization => ProviderErrorKind::Protocol,
    };
    let provider_error = provider_error(kind, UpstreamSendState::NotSent);
    if kind != ProviderErrorKind::InvalidRequest {
        return provider_error;
    }
    provider_error.with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
        error.to_string(),
        Some("invalid_request_normalization".to_owned()),
        Some("invalid_request_error".to_owned()),
    ))
}

/// 将选择阶段失败映射为带结构化 code 与 retry_after 的 Provider 错误。
fn map_selection_error(error: GrokSessionSelectorError) -> ProviderError {
    let (retry_after, message, code) = match error {
        GrokSessionSelectorError::AccountCoolingDown { retry_after } => (
            retry_after,
            cooling_down_message(retry_after),
            "account_cooling_down",
        ),
        GrokSessionSelectorError::ModelCoolingDown { retry_after } => (
            retry_after,
            model_cooling_down_message(retry_after),
            "model_cooling_down",
        ),
        GrokSessionSelectorError::CapacityUnavailable { retry_after } => (
            retry_after,
            "account is at its concurrency or request-interval limit".to_owned(),
            "account_capacity_busy",
        ),
        GrokSessionSelectorError::NoEligibleSession => (
            None,
            "no account is eligible for the requested model".to_owned(),
            "no_eligible_account",
        ),
        GrokSessionSelectorError::Unavailable => (
            None,
            "account scheduling state is temporarily unreadable".to_owned(),
            "account_selector_unavailable",
        ),
        GrokSessionSelectorError::InvalidSession => {
            return provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent);
        }
    };
    let error = provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent);
    let error = match retry_after {
        Some(retry_after) => error.with_retry_after(retry_after),
        None => error,
    };
    error.with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
        message,
        Some(code.to_owned()),
        Some("account_unavailable_error".to_owned()),
    ))
}

fn cooling_down_message(retry_after: Option<Duration>) -> String {
    match retry_after {
        Some(retry_after) => format!(
            "account is cooling down after an upstream failure; retry in {}s",
            retry_after.as_secs().saturating_add(1)
        ),
        None => "account is cooling down after an upstream failure".to_owned(),
    }
}

fn model_cooling_down_message(retry_after: Option<Duration>) -> String {
    match retry_after {
        Some(retry_after) => format!(
            "all eligible accounts are cooling down for this model; retry in {}s",
            retry_after.as_secs().saturating_add(1)
        ),
        None => "all eligible accounts are cooling down for this model".to_owned(),
    }
}

fn map_transport_error_for_context(
    error: GrokInferenceTransportError,
    context: &AttemptContext,
) -> ProviderError {
    let allow_explicit_replay = context.continuation().is_none()
        || error.kind() == GrokInferenceTransportErrorKind::Unauthorized;
    map_transport_error_with_state(error, None, allow_explicit_replay)
}

fn map_stream_error(error: GrokInferenceTransportError) -> ProviderError {
    map_transport_error_with_state(error, Some(UpstreamSendState::Sent), false)
}

fn map_transport_error_with_state(
    error: GrokInferenceTransportError,
    forced_send_state: Option<UpstreamSendState>,
    allow_explicit_replay: bool,
) -> ProviderError {
    let transport_kind = error.kind();
    let kind = match transport_kind {
        GrokInferenceTransportErrorKind::InvalidRequest => ProviderErrorKind::InvalidRequest,
        GrokInferenceTransportErrorKind::Unsupported => ProviderErrorKind::Unsupported,
        GrokInferenceTransportErrorKind::Unauthorized => ProviderErrorKind::Unauthorized,
        GrokInferenceTransportErrorKind::PermissionDenied
        | GrokInferenceTransportErrorKind::ModelAccessDenied
        | GrokInferenceTransportErrorKind::PaymentRequired
        | GrokInferenceTransportErrorKind::SafetyRejected => ProviderErrorKind::PermissionDenied,
        GrokInferenceTransportErrorKind::RateLimited => ProviderErrorKind::RateLimited,
        GrokInferenceTransportErrorKind::QuotaExhausted
        | GrokInferenceTransportErrorKind::FreeQuotaExhausted
        | GrokInferenceTransportErrorKind::ModelQuotaExhausted => ProviderErrorKind::QuotaExhausted,
        GrokInferenceTransportErrorKind::Timeout => ProviderErrorKind::Timeout,
        GrokInferenceTransportErrorKind::Transport => ProviderErrorKind::Transport,
        GrokInferenceTransportErrorKind::Protocol => ProviderErrorKind::Protocol,
        GrokInferenceTransportErrorKind::Unavailable => ProviderErrorKind::Unavailable,
        GrokInferenceTransportErrorKind::Cancelled => ProviderErrorKind::Cancelled,
    };
    let mut mapped = provider_error(
        kind,
        forced_send_state.unwrap_or_else(|| error.send_state()),
    );
    if let Some(status) = error.status() {
        mapped = mapped.with_status(status);
        if allow_explicit_replay
            && forced_send_state.is_none()
            && explicit_rejection_is_replay_safe(transport_kind, status)
        {
            mapped = mapped.with_replay_safe();
        }
    }
    if let Some(retry_after) = error.retry_after() {
        mapped = mapped.with_retry_after(retry_after);
    }
    if let Some(request_id) = error.request_id().cloned() {
        mapped = mapped.with_upstream_request_id(request_id);
    }
    if let Some(code) = error.upstream_code().cloned() {
        mapped = mapped.with_upstream_code(code);
    }
    if let Some(detail) = error.client_visible_upstream_error().cloned() {
        mapped = mapped.with_client_visible_upstream_error(detail);
    }
    if error.requires_credential_recovery() {
        mapped = mapped.with_credential_recovery().with_replay_safe();
    }
    if error.sensitive_context_was_redacted() {
        mapped = mapped.redact_sensitive_context("upstream transport context");
    }
    mapped
}

fn explicit_rejection_is_replay_safe(kind: GrokInferenceTransportErrorKind, status: u16) -> bool {
    matches!(
        (kind, status),
        (GrokInferenceTransportErrorKind::Unauthorized, 401)
            | (
                GrokInferenceTransportErrorKind::QuotaExhausted,
                402 | 403 | 429
            )
            | (
                GrokInferenceTransportErrorKind::FreeQuotaExhausted,
                402 | 403 | 429
            )
            | (
                GrokInferenceTransportErrorKind::ModelQuotaExhausted,
                402 | 403 | 429
            )
            | (GrokInferenceTransportErrorKind::PaymentRequired, 402)
            | (GrokInferenceTransportErrorKind::ModelAccessDenied, 403)
            | (GrokInferenceTransportErrorKind::RateLimited, 429)
    )
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
const QUOTA_CATALOG_INTERVAL: Duration = Duration::from_secs(5 * 60);
// grok2api 对 Free/未知额度耗尽采用 24 小时恢复探测；当前公共账号状态没有
// 独立的 paid period-end 字段，因此 Build 账号统一使用同一保守下限。
const QUOTA_PERIODIC_REFRESH_MIN_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const CLI_RELEASE_WORKER_OWNER: &str = "xai-cli-release";

pub(crate) fn worker_contributions(
    refresh: Arc<GrokCredentialRefreshService>,
    quota: Arc<GrokCredentialQuotaService>,
    catalog: Arc<GrokCredentialCatalogService>,
    accounts: Arc<dyn ProviderAccountStore>,
    provider_kind: ProviderKind,
    cli_release: Arc<GrokCliReleaseService>,
) -> Result<Vec<WorkerContribution>, WorkerDefinitionError> {
    let refresh_id = WorkerId::try_new(WorkerKind::OAuthRefresh, XAI_PROVIDER_NAME)?;
    let catalog_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, XAI_PROVIDER_NAME)?;
    let release_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, CLI_RELEASE_WORKER_OWNER)?;
    Ok(vec![
        WorkerContribution::Registration(scheduled_registration(
            refresh_id,
            OAUTH_REFRESH_INTERVAL,
            Box::new(XaiOAuthRefreshTask { service: refresh }),
        )?),
        WorkerContribution::Registration(scheduled_registration(
            catalog_id,
            QUOTA_CATALOG_INTERVAL,
            Box::new(XaiQuotaCatalogTask {
                accounts,
                quota,
                catalog,
                provider_kind,
                last_periodic_refresh_at: Mutex::new(BTreeMap::new()),
            }),
        )?),
        WorkerContribution::Registration(scheduled_registration(
            release_id,
            GROK_CLI_RELEASE_POLL_INTERVAL,
            Box::new(XaiCliReleaseTask {
                service: cli_release,
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

struct XaiOAuthRefreshTask {
    service: Arc<GrokCredentialRefreshService>,
}

struct XaiCliReleaseTask {
    service: Arc<GrokCliReleaseService>,
}

impl ScheduledTask for XaiCliReleaseTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let refresh = self.service.refresh();
            tokio::pin!(refresh);
            let result = tokio::select! {
                () = context.cancellation().cancelled() => return Ok(()),
                result = &mut refresh => result,
            };
            if let Err(error) = result {
                tracing::warn!(error = %error, "xAI CLI release check failed");
            }
            Ok(())
        })
    }
}

impl ScheduledTask for XaiOAuthRefreshTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            if context.cancellation().is_cancelled() {
                return Ok(());
            }
            let outcomes = self.service.refresh_due().await.map_err(|error| {
                tracing::error!(error = %error, "xAI OAuth refresh cycle failed");
                WorkerTaskError::safe("xAI OAuth refresh failed")
            })?;
            let failures = outcomes
                .iter()
                .filter(|outcome| {
                    matches!(
                        outcome,
                        GrokCredentialRefreshOutcome::Ambiguous { .. }
                            | GrokCredentialRefreshOutcome::Transient { .. }
                            | GrokCredentialRefreshOutcome::Failed { .. }
                    )
                })
                .count();
            if !outcomes.is_empty() {
                tracing::info!(
                    accounts = outcomes.len(),
                    failures,
                    "xAI OAuth refresh cycle completed"
                );
            }
            if failures > 0 {
                tracing::warn!(failures, "xAI OAuth refresh cycle contained failures");
            }
            Ok(())
        })
    }
}

struct XaiQuotaCatalogTask {
    accounts: Arc<dyn ProviderAccountStore>,
    quota: Arc<GrokCredentialQuotaService>,
    catalog: Arc<GrokCredentialCatalogService>,
    provider_kind: ProviderKind,
    last_periodic_refresh_at: Mutex<BTreeMap<ProviderAccountId, Instant>>,
}

impl ScheduledTask for XaiQuotaCatalogTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let accounts = self
                .accounts
                .list_for_provider(&self.provider_kind)
                .await
                .map_err(|_| WorkerTaskError::safe("xAI Provider accounts unavailable"))?;
            let mut failures = 0_u64;
            let now = SystemTime::now();
            let accounts = self.reserve_periodic_refreshes(accounts, now);
            for account in accounts {
                if context.cancellation().is_cancelled() {
                    return Ok(());
                }
                match self.quota.refresh_account(account.id()).await {
                    Ok(_) | Err(GrokQuotaError::AccountUnavailable) => {}
                    Err(_) => failures = failures.saturating_add(1),
                }
            }
            match self.catalog.query_models().await {
                Ok(_) | Err(GrokCredentialCatalogError::NoEligibleCredential) => {}
                Err(_) => failures = failures.saturating_add(1),
            }
            if failures == 0 {
                Ok(())
            } else {
                Err(WorkerTaskError::safe(
                    "xAI quota or catalog synchronization failed",
                ))
            }
        })
    }
}

impl XaiQuotaCatalogTask {
    fn reserve_periodic_refreshes(
        &self,
        accounts: Vec<ProviderAccount>,
        now: SystemTime,
    ) -> Vec<ProviderAccount> {
        let candidates = accounts
            .into_iter()
            .filter(|account| eligible_quota_worker_account(account, now))
            .collect::<Vec<_>>();
        let candidate_ids = candidates
            .iter()
            .map(|account| account.id().clone())
            .collect::<BTreeSet<_>>();
        let now = Instant::now();
        let mut last_periodic_refresh_at = self
            .last_periodic_refresh_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        last_periodic_refresh_at.retain(|account_id, _| candidate_ids.contains(account_id));
        candidates
            .into_iter()
            .filter(|account| {
                let due = last_periodic_refresh_at
                    .get(account.id())
                    .is_none_or(|last| {
                        now.saturating_duration_since(*last) >= QUOTA_PERIODIC_REFRESH_MIN_INTERVAL
                    });
                if due {
                    last_periodic_refresh_at.insert(account.id().clone(), now);
                }
                due
            })
            .collect()
    }
}

fn eligible_quota_worker_account(account: &ProviderAccount, now: SystemTime) -> bool {
    account.enabled()
        && account
            .access_token_expires_at()
            .is_some_and(|expires_at| expires_at > now)
        && account.availability() == AccountAvailability::QuotaExhausted
}
