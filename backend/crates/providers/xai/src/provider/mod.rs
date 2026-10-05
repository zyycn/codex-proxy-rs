//! 官方 Grok Build 会话的 `gateway-core` Provider adapter

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
use gateway_core::engine::middleware::MiddlewareHeader;
use gateway_core::engine::provider::{
    EventStream, Provider, ProviderCallMetadata, ProviderRequest, ProviderRequestObservation,
    ProviderSelectionObservation, ProviderStream,
};
use gateway_core::engine::{AttemptContext, ContinuationAttempt};
use gateway_core::error::{
    ClientVisibleUpstreamError, ContinuationFailure, ContinuationRecoveryDisposition,
    ProviderError, ProviderErrorKind,
};
use gateway_core::event::{
    GatewayEvent, ProviderEvent, ProviderResponseMetadata, ProviderResponseObservation,
    ProviderResponseTimings, ResponseMeta,
};
use gateway_core::operation::{
    CapabilityRequirements, Feature, GenerateRequest, Operation, OperationKind,
    ProviderSessionState,
};
use gateway_core::routing::{
    ModelCapabilities, ModelPresentation, ProviderCandidate, ProviderCatalogGeneration,
    ProviderKind, ProviderModelCapabilities, SupportLevel, UpstreamModelId,
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
use crate::transport::canonical::{GrokCanonicalDecoder, GrokNativeResponseTranslator};
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
mod upstream_adapter;
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

/// 官方 Grok Build Provider；会话选择与 HTTP SSE transport 均由外部注入
///
/// 每次调用只选择一个 OAuth 会话
/// 仅当 xAI 明确拒绝历史 reasoning 密文时，
/// 允许在同账号、同凭据、同会话绑定上剥离密文后有界重试一次
/// 凭据轮换、
/// endpoint fallback 以及公开 xAI API key 推理都不在该 adapter 内
#[derive(Clone)]
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

struct PreparedGrokAttempt {
    generate: GenerateRequest,
    middleware_headers: Vec<MiddlewareHeader>,
    previous_session: Option<XaiSessionState>,
    upstream_model: UpstreamModelId,
    selected: SelectedGrokSession,
    account_selection_wait_ms: u64,
    context: AttemptContext,
}

struct SelectedGrokAttempt {
    upstream_model: UpstreamModelId,
    selected: SelectedGrokSession,
    account_selection_wait_ms: u64,
    context: AttemptContext,
    frozen_requirements: CapabilityRequirements,
}

impl GrokBuildProvider {
    /// 在显式的会话与 transport 边界上创建 Provider
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
    fn resolve_request_profile(
        &self,
        configuration: &gateway_core::account::OpaqueProviderData,
    ) -> Result<gateway_core::account::OpaqueProviderData, ProviderError> {
        use crate::transport::client_profile::{GrokClientProfileSelection, object};
        let profile = GrokClientProfileSelection::parse(configuration)
            .and_then(|selection| selection.resolve(&self.wire_profile))
            .and_then(|profile| object(&profile))
            .map_err(|_| {
                provider_error(
                    ProviderErrorKind::InvalidRequest,
                    UpstreamSendState::NotSent,
                )
            })?;
        Ok(profile)
    }

    fn name(&self) -> &'static str {
        XAI_PROVIDER_NAME
    }

    fn catalog_generation(&self) -> ProviderCatalogGeneration {
        self.catalog.catalog_generation()
    }

    fn request_observation(
        &self,
        operation: &Operation,
        _client_api_key_id: &gateway_core::policy::ClientApiKeyId,
    ) -> ProviderRequestObservation {
        let Operation::Generate(request) = operation else {
            return ProviderRequestObservation::default();
        };
        let payload = request.protocol_payload();
        if payload.protocol() != "openai" {
            return ProviderRequestObservation::default();
        }
        let semantics = codex_responses_request_semantics(payload.body(), payload.context());
        ProviderRequestObservation {
            requested_model: None,
            reasoning_effort: semantics.reasoning_effort,
            reasoning_preset: semantics.reasoning_preset.map(str::to_owned),
            request_kind: semantics.request_kind,
            subagent_kind: semantics.subagent_kind,
            compact: semantics.compact,
            continuation: Default::default(),
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
        self: Arc<Self>,
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
    fn request_wire_profile(
        &self,
        context: &AttemptContext,
    ) -> Result<XaiWireProfileState, ProviderError> {
        let profile = match context.request_profile() {
            Some(profile) => serde_json::from_value(serde_json::Value::Object(
                profile.expose_to_provider().clone(),
            ))
            .map_err(|_| {
                provider_error(
                    ProviderErrorKind::InvalidRequest,
                    UpstreamSendState::NotSent,
                )
            })?,
            None => self.wire_profile.snapshot(),
        };
        Ok(XaiWireProfileState::new(profile))
    }

    async fn execute_generate(
        self: Arc<Self>,
        generate: &GenerateRequest,
        candidate: &ProviderCandidate,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        let adapter =
            context.upstream_adapter(candidate.provider(), candidate_upstream_model(candidate)?)?;
        if adapter.is_none()
            && generate
                .provider_session_state(XAI_PROVIDER_NAME)
                .is_some_and(|state| state.extension_owner().is_some())
        {
            return Err(invalid_continuation());
        }
        let selected = self
            .select_grok_attempt(generate, candidate, context, adapter.is_some())
            .await?;
        let provider_kind =
            ProviderKind::new(XAI_PROVIDER_NAME).map_err(|_| protocol_not_sent())?;
        let account_id = selected.selected.account_id().clone();
        let model = selected.upstream_model.as_str().to_owned();
        let middleware_context = selected.context.clone();
        let provider = Arc::clone(&self);
        middleware_context
            .execute_middleware(
                Operation::Generate(generate.clone()),
                provider_kind,
                Some(model),
                account_id,
                Box::new(move |operation, middleware_headers| {
                    Box::pin(async move {
                        if let Some(adapter) = adapter {
                            return provider.execute_upstream_adapter(
                                operation,
                                middleware_headers,
                                selected,
                                adapter,
                            );
                        }
                        provider
                            .execute_selected_grok(operation, middleware_headers, selected)
                            .await
                    })
                }),
            )
            .await
    }

    async fn select_grok_attempt(
        &self,
        generate: &GenerateRequest,
        candidate: &ProviderCandidate,
        context: AttemptContext,
        adapted: bool,
    ) -> Result<SelectedGrokAttempt, ProviderError> {
        let upstream_model = candidate_upstream_model(candidate)?;
        let previous_session = if adapted {
            None
        } else {
            decode_xai_session_state(generate)?
        };
        let native_source = !adapted && generate.protocol_payload().protocol() == "openai";
        let native_compaction = native_source
            && crate::transport::compaction::has_terminal_compaction_trigger(generate);
        let (selection_model, operation_account, affinity) = if native_compaction {
            validate_compaction_context(&context)?;
            let operation_account = previous_session_account(previous_session.as_ref())?;
            let request = GrokCompactionRequest::encode(
                generate,
                upstream_model.as_str(),
                context.client_api_key_ref(),
            )
            .map_err(map_request_error)?;
            let selection_model = request
                .upstream_model()
                .ok_or_else(protocol_not_sent)?
                .to_owned();
            (
                selection_model,
                merge_frozen_account_owner(&context, operation_account)?,
                request.affinity().cloned(),
            )
        } else if native_source {
            let operation_account = continuation_account(&context, previous_session.as_ref())?;
            let request = GrokResponsesRequest::encode(
                generate,
                upstream_model.as_str(),
                context.client_api_key_ref(),
            )
            .map_err(map_request_error)?;
            let selection_model = request
                .upstream_model()
                .ok_or_else(protocol_not_sent)?
                .to_owned();
            (
                selection_model,
                merge_frozen_account_owner(&context, operation_account)?,
                request.affinity().cloned(),
            )
        } else {
            // 源协议由转换插件拥有；这里只使用冻结的候选模型与 Core 账号 owner，
            // 不把未知 JSON 中的同名字段解释为 xAI 会话或亲和事实
            let operation_account = continuation_account(&context, previous_session.as_ref())?;
            (
                upstream_model.as_str().to_owned(),
                merge_frozen_account_owner(&context, operation_account)?,
                None,
            )
        };
        let selection_model =
            UpstreamModelId::new(selection_model).map_err(|_| protocol_not_sent())?;
        let selection_started_at = Instant::now();
        let selected = select_grok_session(
            self.selector.as_ref(),
            candidate,
            &selection_model,
            &context,
            operation_account,
            affinity,
        )
        .await?;
        let account_selection_wait_ms =
            u64::try_from(selection_started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(SelectedGrokAttempt {
            upstream_model: upstream_model.clone(),
            selected,
            account_selection_wait_ms,
            context,
            frozen_requirements: Operation::Generate(generate.clone()).capability_requirements(),
        })
    }

    async fn execute_selected_grok(
        &self,
        operation: Operation,
        middleware_headers: Vec<MiddlewareHeader>,
        selected: SelectedGrokAttempt,
    ) -> Result<ProviderStream, ProviderError> {
        let SelectedGrokAttempt {
            upstream_model,
            selected,
            account_selection_wait_ms,
            context,
            frozen_requirements,
        } = selected;
        let Operation::Generate(generate) = operation else {
            return Err(protocol_not_sent());
        };
        if generate.protocol_payload().protocol() != "openai"
            || native_request_requirements(&generate) != frozen_requirements
        {
            return Err(provider_error(
                ProviderErrorKind::InvalidRequest,
                UpstreamSendState::NotSent,
            ));
        }
        let previous_session = decode_xai_session_state(&generate)?;
        let operation_account =
            if crate::transport::compaction::has_terminal_compaction_trigger(&generate) {
                validate_compaction_context(&context)?;
                previous_session_account(previous_session.as_ref())?
            } else {
                continuation_account(&context, previous_session.as_ref())?
            };
        let operation_account = merge_frozen_account_owner(&context, operation_account)?;
        validate_selected_grok_account(&selected, &context, operation_account.as_ref())?;
        let prepared = PreparedGrokAttempt {
            generate,
            middleware_headers,
            previous_session,
            upstream_model,
            selected,
            account_selection_wait_ms,
            context,
        };
        if crate::transport::compaction::has_terminal_compaction_trigger(&prepared.generate) {
            self.execute_prepared_compaction(prepared).await
        } else {
            self.execute_prepared_generate(prepared).await
        }
    }

    async fn execute_prepared_generate(
        &self,
        prepared: PreparedGrokAttempt,
    ) -> Result<ProviderStream, ProviderError> {
        let PreparedGrokAttempt {
            generate,
            middleware_headers,
            previous_session,
            upstream_model,
            selected,
            account_selection_wait_ms,
            context,
        } = prepared;
        let mut upstream_request = GrokResponsesRequest::encode(
            &generate,
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
        let instructions = upstream_request.instructions().cloned();
        if let Some(previous) = previous_session.as_ref() {
            upstream_request.inherit_session(previous.session_id.as_deref());
        }
        // 首字计时的起点：账号选择完成之后、上游建立之前
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
        let native_response_translator =
            GrokNativeResponseTranslator::for_request(&upstream_request);
        let reasoning_replay_capture =
            reasoning_replay_key.map(|key| self.reasoning_replay.capture(key));
        let session_capture = (!matches!(
            context.continuation(),
            Some(ContinuationBinding::Pinned(_) | ContinuationBinding::External(_))
        ) || previous_session.is_some())
        .then(|| GrokSessionCapture {
            previous: previous_session,
            request_input,
            instructions,
            response_stored: upstream_request
                .body()
                .get("store")
                .and_then(Value::as_bool)
                == Some(true),
            account_id: selected.account_id().as_str().to_owned(),
            session_id: upstream_request.session_id().map(str::to_owned),
            output_items: BTreeMap::new(),
        });
        let selected = Arc::new(selected);
        let allows_account_state_mutation = selected.allows_account_state_mutation();
        let metadata =
            provider_call_metadata(&upstream_model, &selected, account_selection_wait_ms)?;
        let events = cold_http_sse_stream(
            Arc::clone(&self.selector),
            Arc::clone(&self.transport),
            GrokStreamAttempt {
                client_identity: self.client_identity.clone(),
                wire_profile: self.request_wire_profile(&context)?,
                credential_recovery: Arc::clone(&self.credential_recovery),
                responses_url: self.responses_url.clone(),
                request: upstream_request,
                middleware_headers,
                upstream_model: wire_upstream_model,
                context,
                session: Arc::clone(&selected),
                output_started_at,
                native_response_boundary: true,
                session_capture,
                reasoning_replay_capture,
            },
        );
        let stream = ProviderStream::new(metadata, events, selected);
        let stream = if allows_account_state_mutation {
            stream.with_account_feedback(Arc::clone(&self.account_feedback))
        } else {
            stream
        };
        Ok(stream.with_native_response_translator(native_response_translator))
    }

    async fn execute_prepared_compaction(
        &self,
        prepared: PreparedGrokAttempt,
    ) -> Result<ProviderStream, ProviderError> {
        let PreparedGrokAttempt {
            generate,
            middleware_headers,
            previous_session,
            upstream_model,
            selected,
            account_selection_wait_ms,
            context,
        } = prepared;
        validate_compaction_context(&context)?;
        let inherited_session_id = previous_session
            .as_ref()
            .and_then(|previous| previous.session_id.clone());
        let upstream_request = GrokCompactionRequest::encode(
            &generate,
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
        let selected = Arc::new(selected);
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
        let metadata =
            provider_call_metadata(&upstream_model, &selected, account_selection_wait_ms)?;
        let events = cold_compaction_http_sse_stream(
            Arc::clone(&self.selector),
            Arc::clone(&self.transport),
            GrokCompactionStreamAttempt {
                client_identity: self.client_identity.clone(),
                wire_profile: self.request_wire_profile(&context)?,
                credential_recovery: Arc::clone(&self.credential_recovery),
                responses_url: self.responses_url.clone(),
                request: upstream_request,
                middleware_headers,
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

fn native_request_requirements(request: &GenerateRequest) -> CapabilityRequirements {
    // 这里只读取本 Provider 已知的 Responses 字段；工具别名等原生保护字段另行校验
    Operation::Generate(GenerateRequest::from_protocol_payload(
        request.protocol_payload().clone(),
    ))
    .capability_requirements()
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
    .with_cancellation(context.cancellation().clone())
    .with_concurrency_wait_budget(context.concurrency_wait_budget().clone())
    .with_request_policy(
        context.request_policy_context().cloned(),
        context.attempt_index(),
    )
    .with_eligibility_policy(if context.is_diagnostic_required_account() {
        AccountEligibilityPolicy::BypassForDiagnostic
    } else {
        AccountEligibilityPolicy::Enforce
    })
    .with_affinity(affinity);
    if context.deadline().is_elapsed() {
        return Err(provider_error(
            ProviderErrorKind::Timeout,
            UpstreamSendState::NotSent,
        ));
    }
    let cancellation = context.cancellation().clone();
    let selected = tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(provider_error(
            ProviderErrorKind::Cancelled,
            UpstreamSendState::NotSent,
        )),
        _ = context.deadline().wait() => Err(provider_error(
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
    upstream_model: &UpstreamModelId,
    selected: &SelectedGrokSession,
    account_selection_wait_ms: u64,
) -> Result<ProviderCallMetadata, ProviderError> {
    Ok(ProviderCallMetadata::new(
        ProviderKind::new(XAI_PROVIDER_NAME).map_err(|_| protocol_not_sent())?,
        upstream_model.clone(),
        selected.account_id().clone(),
        UpstreamTransport::new(HTTP_SSE_TRANSPORT).map_err(|_| protocol_not_sent())?,
    )
    .with_selection_observation(ProviderSelectionObservation::new(
        account_selection_wait_ms,
        selected.capacity_snapshot(),
    )))
}

fn validate_compaction_context(context: &AttemptContext) -> Result<(), ProviderError> {
    if context.continuation().is_some()
        || context.continuation_attempt() != ContinuationAttempt::None
    {
        return Err(provider_error(
            ProviderErrorKind::InvalidRequest,
            UpstreamSendState::NotSent,
        ));
    }
    Ok(())
}

fn previous_session_account(
    previous_session: Option<&XaiSessionState>,
) -> Result<Option<ProviderAccountId>, ProviderError> {
    previous_session
        .map(|previous| ProviderAccountId::new(previous.account_id.clone()))
        .transpose()
        .map_err(|_| protocol_not_sent())
}

fn merge_frozen_account_owner(
    context: &AttemptContext,
    operation_account: Option<ProviderAccountId>,
) -> Result<Option<ProviderAccountId>, ProviderError> {
    if context.continuation_attempt() == ContinuationAttempt::ReplayAny {
        return Ok(operation_account);
    }
    let provider = ProviderKind::new(XAI_PROVIDER_NAME).map_err(|_| protocol_not_sent())?;
    let Some(owner) = context
        .account_state_owner()
        .filter(|owner| owner.provider() == &provider)
    else {
        return Ok(operation_account);
    };
    if operation_account
        .as_ref()
        .is_some_and(|account| account != owner.account())
    {
        return Err(invalid_continuation());
    }
    Ok(Some(owner.account().clone()))
}

fn validate_selected_grok_account(
    selected: &SelectedGrokSession,
    context: &AttemptContext,
    operation_account: Option<&ProviderAccountId>,
) -> Result<(), ProviderError> {
    if context.excluded_accounts().contains(selected.account_id())
        || context
            .required_account()
            .is_some_and(|required| required != selected.account_id())
        || operation_account.is_some_and(|required| required != selected.account_id())
    {
        return Err(protocol_not_sent());
    }
    Ok(())
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
        // 请求 adapter 会在发送前规范化仅客户端侧的工具结构
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
    .with_max_context_window_tokens(Some(500_000))
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
    // Grok 目录只声明一个窗口，继续将它作为客户端可覆盖上限
    .with_max_context_window_tokens(context_window_tokens)
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
