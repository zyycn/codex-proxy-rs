//! 官方 Grok Build 会话的 `gateway-core` Provider adapter。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use futures::{StreamExt, future::BoxFuture};
use gateway_core::account::{
    AccountEligibilityPolicy, AccountFeedbackStats, ProviderAccount, ProviderAccountId,
    ProviderAccountStore,
};
use gateway_core::engine::continuation::ContinuationBinding;
use gateway_core::engine::provider::{
    EventStream, Provider, ProviderCallMetadata, ProviderCatalogGeneration,
    ProviderModelCapabilities, ProviderRequest, ProviderRequestObservation,
    ProviderSelectionObservation, ProviderStream,
};
use gateway_core::engine::{AttemptContext, ContinuationAttempt};
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
use gateway_core::upstream::{UpstreamSendState, UpstreamTransport};
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

mod continuation;
mod failure;
mod stream;
mod workers;

use continuation::*;
use failure::*;
use stream::*;
pub(crate) use workers::worker_contributions;

const HTTP_SSE_TRANSPORT: &str = "http_sse";
const DEFAULT_GROK_MODEL: &str = "grok-4.5";
const XAI_SESSION_STATE_MAX_BYTES: usize = 8 * 1024 * 1024;
const XAI_SESSION_OUTPUT_LIMIT: usize = 4_096;
const REASONING_DECODE_FAILED_CODE: &str = "reasoning_decode_failed";
const RESPONSE_NOT_FOUND_CODE: &str = "not_found";

/// 官方 Grok Build Provider；会话选择与 HTTP SSE transport 均由外部注入。
///
/// 每次调用只选择一个 OAuth 会话。仅当 xAI 明确拒绝历史 reasoning 密文时，
/// 允许在同账号、同凭据、同会话绑定上剥离密文后有界重试一次。凭据轮换、
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
        let upstream_model = candidate_upstream_model(candidate)?;
        let previous_session = decode_xai_session_state(generate)?;
        let continuation_account = continuation_account(&context, previous_session.as_ref())?;
        let mut upstream_request = GrokResponsesRequest::encode(
            generate,
            upstream_model.as_str(),
            context.client_api_key_ref(),
        )
        .map_err(map_request_error)?;
        let wire_upstream_model = UpstreamModelId::new(
            upstream_request
                .upstream_model()
                .ok_or_else(protocol_not_sent)?
                .to_owned(),
        )
        .map_err(|_| protocol_not_sent())?;
        let request_input = upstream_request.input_items();
        if let Some(previous) = previous_session.as_ref() {
            upstream_request.inherit_session(previous.session_id.as_deref());
        }
        let selection_started_at = Instant::now();
        let selected = select_grok_session(
            self.selector.as_ref(),
            candidate,
            &wire_upstream_model,
            &context,
            continuation_account,
            upstream_request.affinity().cloned(),
        )
        .await?;
        let account_selection_wait_ms =
            u64::try_from(selection_started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
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
                    wire_upstream_model.as_str(),
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
        let metadata = provider_call_metadata(candidate, &selected, account_selection_wait_ms)?;
        let events = cold_http_sse_stream(
            Arc::clone(&self.selector),
            Arc::clone(&self.transport),
            GrokStreamAttempt {
                client_identity: self.client_identity.clone(),
                wire_profile: self.wire_profile.clone(),
                credential_recovery: Arc::clone(&self.credential_recovery),
                responses_url: self.responses_url.clone(),
                request: upstream_request,
                upstream_model: wire_upstream_model,
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
        let upstream_model = candidate_upstream_model(candidate)?;
        let upstream_request = GrokCompactionRequest::encode(
            generate,
            upstream_model.as_str(),
            context.client_api_key_ref(),
        )
        .map_err(map_request_error)?;
        let wire_upstream_model = UpstreamModelId::new(
            upstream_request
                .upstream_model()
                .ok_or_else(protocol_not_sent)?
                .to_owned(),
        )
        .map_err(|_| protocol_not_sent())?;
        let explicit_replay_session_id = upstream_request
            .reasoning_replay_session_id()
            .map(str::to_owned);
        let upstream_session_id = inherited_session_id
            .clone()
            .or_else(|| explicit_replay_session_id.clone());
        let selection_started_at = Instant::now();
        let selected = Arc::new(
            select_grok_session(
                self.selector.as_ref(),
                candidate,
                &wire_upstream_model,
                &context,
                operation_account,
                upstream_request.affinity().cloned(),
            )
            .await?,
        );
        let account_selection_wait_ms =
            u64::try_from(selection_started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        let reasoning_replay_key = explicit_replay_session_id
            .as_deref()
            .or(inherited_session_id.as_deref())
            .and_then(|session_id| {
                self.reasoning_replay.key(
                    wire_upstream_model.as_str(),
                    session_id,
                    selected.account_id().as_str(),
                )
            });
        let allows_account_state_mutation = selected.allows_account_state_mutation();
        let metadata = provider_call_metadata(candidate, &selected, account_selection_wait_ms)?;
        let events = cold_compaction_http_sse_stream(
            Arc::clone(&self.selector),
            Arc::clone(&self.transport),
            GrokCompactionStreamAttempt {
                client_identity: self.client_identity.clone(),
                wire_profile: self.wire_profile.clone(),
                credential_recovery: Arc::clone(&self.credential_recovery),
                responses_url: self.responses_url.clone(),
                request: upstream_request,
                upstream_model: wire_upstream_model,
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
    wire_upstream_model: &UpstreamModelId,
    context: &AttemptContext,
    operation_account: Option<ProviderAccountId>,
    affinity: Option<GrokSessionAffinityKey>,
) -> Result<SelectedGrokSession, ProviderError> {
    let required_account = context.required_account().cloned().or(operation_account);
    let selection = GrokSessionSelection::new(
        wire_upstream_model.clone(),
        context.excluded_accounts().clone(),
        required_account.clone(),
        context.account_selection_policy(),
        context.deadline(),
        Arc::clone(candidate.account_scope()),
        context.client_api_key_ref().clone(),
    )
    .with_eligibility_policy(if context.is_diagnostic_required_account() {
        AccountEligibilityPolicy::BypassForDiagnostic
    } else {
        AccountEligibilityPolicy::Enforce
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
    account_selection_wait_ms: u64,
) -> Result<ProviderCallMetadata, ProviderError> {
    Ok(ProviderCallMetadata::new(
        ProviderKind::new(XAI_PROVIDER_NAME).map_err(|_| protocol_not_sent())?,
        candidate_upstream_model(candidate)?.clone(),
        selected.account_id().clone(),
        UpstreamTransport::new(HTTP_SSE_TRANSPORT).map_err(|_| protocol_not_sent())?,
    )
    .with_selection_observation(ProviderSelectionObservation::new(
        account_selection_wait_ms,
        selected.capacity_snapshot(),
    )))
}

fn candidate_upstream_model(
    candidate: &ProviderCandidate,
) -> Result<&UpstreamModelId, ProviderError> {
    candidate.upstream_model().ok_or_else(protocol_not_sent)
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
