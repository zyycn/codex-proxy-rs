//! `gateway-core` Provider adapter for official Grok Build sessions.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use base64::Engine as _;
use futures::{StreamExt, future::BoxFuture};
use gateway_core::engine::continuation::ContinuationBinding;
use gateway_core::engine::credential::{
    AccountAvailability, ProviderAccount, ProviderAccountId, ProviderAccountStore,
};
use gateway_core::engine::provider::{
    EventStream, Provider, ProviderCallMetadata, ProviderCatalogGeneration,
    ProviderModelCapabilities, ProviderRequest, ProviderStream, UpstreamTransport,
};
use gateway_core::engine::{AttemptContext, ContinuationAttempt, UpstreamSendState};
use gateway_core::error::{ContinuationFailure, ProviderError, ProviderErrorKind};
use gateway_core::event::{
    CompactionOutput, GatewayEvent, ProviderEvent, ProviderResponseObservation, ResponseMeta,
};
use gateway_core::operation::{
    CompactConversationRequest, Feature, GenerateRequest, Operation, OperationKind,
    ProviderSessionState,
};
use gateway_core::provider_ports::ProviderInstanceCatalogPort;
use gateway_core::routing::{
    InstanceHealth, ModelCapabilities, ProviderCandidate, ProviderInstance, ProviderKind,
    SupportLevel, UpstreamModelId,
};
use gateway_core::task::{
    ScheduledTask, WorkerContribution, WorkerCycleContext, WorkerDefinitionError, WorkerId,
    WorkerKind, WorkerLeaseRequest, WorkerRegistration, WorkerRunnable, WorkerSchedule,
    WorkerTaskError,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::GrokCatalogCapabilityEvidence;
use crate::XaiWireProfileState;
use crate::credential::{
    GrokCredentialCatalogService, GrokCredentialQuotaService, GrokCredentialRecovery,
    GrokCredentialRecoveryOutcome, GrokCredentialRefreshOutcome, GrokCredentialRefreshService,
    GrokQuotaError,
};
use crate::transport::canonical::GrokCanonicalDecoder;
use crate::transport::config::XAI_PROVIDER_NAME;
use crate::transport::headers::{GrokClientIdentity, GrokHeader, build_grok_headers};
use crate::transport::profile::{GROK_CLI_RELEASE_POLL_INTERVAL, GrokCliReleaseService};
use crate::transport::{
    GrokCompactionDecodeError, GrokCompactionRequest, GrokCompactionSummaryDecoder,
    GrokCredentialFailure, GrokInferenceChunkStream, GrokInferenceRequest, GrokInferenceResponse,
    GrokInferenceTransport, GrokInferenceTransportError, GrokInferenceTransportErrorKind,
    GrokProviderConfigError, GrokProviderInstanceConfig, GrokRequestEncodeError,
    GrokResponsesRequest, GrokSessionAffinityKey, GrokSessionSelection, GrokSessionSelector,
    GrokSessionSelectorError, SelectedGrokSession,
};

const HTTP_SSE_TRANSPORT: &str = "http_sse";
const XAI_SESSION_STATE_MAX_BYTES: usize = 8 * 1024 * 1024;
const XAI_SESSION_OUTPUT_LIMIT: usize = 4_096;
const MIN_REASONING_CIPHERTEXT_BYTES: usize = 50;
const MIN_REASONING_CIPHERTEXT_ENTROPY: f64 = 0.85;
const REASONING_DECODE_FAILED_CODE: &str = "reasoning_decode_failed";
const RESPONSE_NOT_FOUND_CODE: &str = "not_found";

/// Official Grok Build provider with injected session selection and HTTP SSE
/// transport ports.
///
/// Each call selects exactly one OAuth session and prepares exactly one visible
/// upstream POST. Retries, credential rotation, endpoint fallback, and public
/// xAI API-key inference are deliberately outside this adapter.
pub struct GrokBuildProvider {
    selector: Arc<dyn GrokSessionSelector>,
    transport: Arc<dyn GrokInferenceTransport>,
    catalog: Arc<GrokCredentialCatalogService>,
    credential_recovery: Arc<dyn GrokCredentialRecovery>,
    client_identity: GrokClientIdentity,
    wire_profile: XaiWireProfileState,
}

impl GrokBuildProvider {
    /// Creates a provider over explicit session and transport boundaries.
    #[must_use]
    pub fn new(
        selector: Arc<dyn GrokSessionSelector>,
        transport: Arc<dyn GrokInferenceTransport>,
        catalog: Arc<GrokCredentialCatalogService>,
        credential_recovery: Arc<dyn GrokCredentialRecovery>,
        wire_profile: XaiWireProfileState,
    ) -> Self {
        Self {
            selector,
            transport,
            catalog,
            credential_recovery,
            client_identity: GrokClientIdentity::new(),
            wire_profile,
        }
    }

    /// Validates an instance before publishing its runtime snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error unless the instance uses the exact official Grok CLI
    /// proxy endpoint and v1 HTTP SSE option schema.
    pub fn validate_instance(
        instance: &ProviderInstance,
    ) -> Result<GrokProviderInstanceConfig, GrokProviderConfigError> {
        GrokProviderInstanceConfig::from_snapshot(instance)
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

    async fn query_model_capabilities(
        &self,
        instance: &ProviderInstance,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        let models = self
            .catalog
            .query_instance_models(instance)
            .await
            .map_err(|_| {
                provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent)
            })?;
        Ok(models
            .into_iter()
            .map(|model| {
                let mut operations = BTreeSet::new();
                if model.capabilities().responses_api()
                    == GrokCatalogCapabilityEvidence::DeclaredNative
                {
                    operations.insert(OperationKind::Generate);
                    operations.insert(OperationKind::CompactConversation);
                }
                let capabilities = ModelCapabilities::new(
                    operations,
                    model
                        .limits()
                        .context_window_tokens()
                        .map_or(0, std::num::NonZeroU64::get),
                    model
                        .limits()
                        .max_output_tokens()
                        .map(std::num::NonZeroU64::get),
                )
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
            })
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
        let instance_snapshot = candidate.instance_snapshot();
        if instance_snapshot.id() != candidate.instance() {
            return Err(provider_error(
                ProviderErrorKind::Protocol,
                UpstreamSendState::NotSent,
            ));
        }
        let instance = GrokProviderInstanceConfig::from_snapshot(instance_snapshot)
            .map_err(map_instance_config_error)?;
        preflight_context(&context)?;

        match request.operation() {
            Operation::Generate(generate) => {
                self.execute_generate(generate, candidate, instance, context)
                    .await
            }
            Operation::CompactConversation(compact) => {
                self.execute_compaction(compact, candidate, instance, context)
                    .await
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
        instance: GrokProviderInstanceConfig,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        let previous_session = decode_xai_session_state(generate)?;
        let continuation_account =
            continuation_account(&context, candidate.instance(), previous_session.as_ref())?;
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
        apply_continuation(
            &mut upstream_request,
            previous_session.as_ref(),
            &context,
            candidate.instance(),
            selected.account_id(),
            request_input.as_slice(),
        )?;
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
        let metadata = provider_call_metadata(candidate, &selected)?;
        let events = cold_http_sse_stream(
            Arc::clone(&self.selector),
            Arc::clone(&self.transport),
            GrokStreamAttempt {
                client_identity: self.client_identity.clone(),
                wire_profile: self.wire_profile.clone(),
                credential_recovery: Arc::clone(&self.credential_recovery),
                instance,
                request: upstream_request,
                upstream_model: candidate.upstream_model().clone(),
                context,
                session: Arc::clone(&selected),
                session_capture,
            },
        );
        Ok(ProviderStream::new(metadata, events, selected))
    }

    async fn execute_compaction(
        &self,
        compact: &CompactConversationRequest,
        candidate: &ProviderCandidate,
        instance: GrokProviderInstanceConfig,
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
        let previous_session = decode_xai_session_state(compact.generation())?;
        let operation_account = previous_session
            .as_ref()
            .map(|previous| ProviderAccountId::new(previous.account_id.clone()))
            .transpose()
            .map_err(|_| protocol_not_sent())?;
        let upstream_session_id = previous_session
            .as_ref()
            .and_then(|previous| previous.session_id.clone());
        let upstream_request = GrokCompactionRequest::encode(
            compact,
            candidate.upstream_model().as_str(),
            context.client_api_key_ref(),
        )
        .map_err(map_request_error)?;
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
        let metadata = provider_call_metadata(candidate, &selected)?;
        let events = cold_compaction_http_sse_stream(
            Arc::clone(&self.selector),
            Arc::clone(&self.transport),
            GrokCompactionStreamAttempt {
                client_identity: self.client_identity.clone(),
                wire_profile: self.wire_profile.clone(),
                credential_recovery: Arc::clone(&self.credential_recovery),
                instance,
                request: upstream_request,
                upstream_model: candidate.upstream_model().clone(),
                upstream_session_id,
                context,
                session: Arc::clone(&selected),
            },
        );
        Ok(ProviderStream::new(metadata, events, selected))
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
        candidate.instance().clone(),
        candidate.upstream_model().clone(),
        context.excluded_accounts().clone(),
        required_account.clone(),
        context.account_selection_policy(),
        context.deadline(),
    )
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
        candidate.instance().clone(),
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
        // Grok Build's Responses tool protocol remains available when the
        // optional catalog field is omitted; the request adapter normalizes
        // the client-only tool shapes before sending.
        GrokCatalogCapabilityEvidence::Unknown => SupportLevel::Emulated,
    }
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
    instance: &gateway_core::routing::ProviderInstanceId,
    previous_session: Option<&XaiSessionState>,
) -> Result<Option<gateway_core::engine::credential::ProviderAccountId>, ProviderError> {
    let Some(continuation) = context.continuation() else {
        return Ok(None);
    };
    match (context.continuation_attempt(), continuation) {
        (ContinuationAttempt::Native, ContinuationBinding::Pinned(pin)) => {
            if pin.provider().as_str() != XAI_PROVIDER_NAME || pin.instance() != instance {
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
    instance: &gateway_core::routing::ProviderInstanceId,
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
            if !pin.matches(&provider, instance, account) {
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

fn valid_reasoning_ciphertext(value: &str) -> bool {
    if value.is_empty()
        || value != value.trim()
        || value.len() > XAI_SESSION_STATE_MAX_BYTES
        || value.starts_with("gAAAA")
        || value.contains('=')
    {
        return false;
    }
    let Ok(decoded) = base64::engine::general_purpose::STANDARD_NO_PAD.decode(value) else {
        return false;
    };
    decoded.len() >= MIN_REASONING_CIPHERTEXT_BYTES
        && byte_entropy_ratio(&decoded) >= MIN_REASONING_CIPHERTEXT_ENTROPY
}

fn byte_entropy_ratio(value: &[u8]) -> f64 {
    if value.is_empty() {
        return 0.0;
    }
    let mut counts = [0_u32; 256];
    for byte in value {
        counts[usize::from(*byte)] += 1;
    }
    let size = value.len() as f64;
    let entropy = counts
        .into_iter()
        .filter(|count| *count > 0)
        .map(|count| {
            let probability = f64::from(count) / size;
            -probability * probability.log2()
        })
        .sum::<f64>();
    let symbols = value.len().min(256);
    if symbols <= 1 {
        return 0.0;
    }
    entropy / (symbols as f64).log2()
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
    transcript.extend(
        capture
            .output_items
            .into_values()
            .filter_map(|item| portable_output_item(item, false))
            .map(|item| XaiReplayItem::AccountOutput {
                account_id: capture.account_id.clone(),
                item,
            }),
    );
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
        && let Some(item) = wire.data().get("item").cloned()
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
        capture.output_items.clear();
        capture.output_items.extend(
            output
                .iter()
                .take(XAI_SESSION_OUTPUT_LIMIT)
                .enumerate()
                .filter_map(|(index, item)| {
                    u32::try_from(index).ok().map(|index| (index, item.clone()))
                }),
        );
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
    instance: GrokProviderInstanceConfig,
    request: GrokResponsesRequest,
    upstream_model: UpstreamModelId,
    context: AttemptContext,
    session: Arc<SelectedGrokSession>,
    session_capture: Option<GrokSessionCapture>,
}

struct GrokCompactionStreamAttempt {
    client_identity: GrokClientIdentity,
    wire_profile: XaiWireProfileState,
    credential_recovery: Arc<dyn GrokCredentialRecovery>,
    instance: GrokProviderInstanceConfig,
    request: GrokCompactionRequest,
    upstream_model: UpstreamModelId,
    upstream_session_id: Option<String>,
    context: AttemptContext,
    session: Arc<SelectedGrokSession>,
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
            let error =
                map_continuation_failure(context, map_transport_error_for_context(error, context));
            let error = recover_or_record_failure(
                selector,
                credential_recovery,
                session,
                error,
                context.credential_recovery_attempted(),
            )
            .await;
            return Err(GrokInferenceStartFailure { observation, error });
        }
    };
    let mut observation = ProviderResponseObservation::new(
        UpstreamTransport::new(HTTP_SSE_TRANSPORT).map_err(|_| GrokInferenceStartFailure {
            observation: None,
            error: protocol_sent(),
        })?,
    )
    .with_http_version(response.http_version())
    .with_status_code(response.status_code());
    if let Some(request_id) = response.request_id().cloned() {
        observation = observation.with_request_id(request_id);
    }
    Ok(AcceptedGrokInference {
        response,
        observation,
    })
}

async fn next_grok_chunk(
    body: &mut GrokInferenceChunkStream,
    selector: &dyn GrokSessionSelector,
    session: &SelectedGrokSession,
    context: &AttemptContext,
) -> Result<Option<Vec<u8>>, ProviderError> {
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
            Some(Err(error)) => {
                let error = map_stream_error(error);
                Err(record_stream_failure(selector, session, error).await)
            }
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
        instance,
        request,
        upstream_model,
        upstream_session_id,
        context,
        session,
    } = attempt;
    Box::pin(async_stream::try_stream! {
        let mut headers = build_grok_headers(
            &wire_profile,
            &session,
            &client_identity,
            context.request_id(),
            upstream_session_id.as_deref(),
            None,
            &upstream_model,
        );
        if let Some(upstream_session_id) = upstream_session_id.as_deref() {
            headers.push(GrokHeader::sensitive(
                "x-grok-session-id",
                crate::credential::SecretValue::new(upstream_session_id.to_owned()),
            ));
        }
        let body = request.to_json_bytes().map_err(map_request_error)?;
        let inference_request = GrokInferenceRequest::new(
            instance.responses_url().clone(),
            headers,
            body,
            session.binding().clone(),
        );
        let accepted = match start_grok_inference(
            selector.as_ref(),
            transport.as_ref(),
            credential_recovery.as_ref(),
            inference_request,
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
        let mut started = None;
        let mut completed = None;
        let mut accounting = Vec::new();

        'stream: while let Some(chunk) = next_grok_chunk(
            &mut body,
            selector.as_ref(),
            &session,
            &context,
        )
        .await
        .map_err(mark_transient_compaction_failure)?
        {
            let events = canonical.push(&chunk).map_err(|error| {
                mark_transient_compaction_failure(map_continuation_failure(&context, error))
            })?;
            for event in events {
                summary.observe(&event).map_err(map_compaction_decode_error)?;
                collect_compaction_facts(
                    &event,
                    &mut started,
                    &mut completed,
                    &mut accounting,
                )?;
                if completed.is_some() {
                    break 'stream;
                }
            }
        }

        if completed.is_none() {
            for event in canonical.finish_without_terminal().map_err(|error| {
                mark_transient_compaction_failure(map_continuation_failure(&context, error))
            })? {
                summary.observe(&event).map_err(map_compaction_decode_error)?;
                collect_compaction_facts(
                    &event,
                    &mut started,
                    &mut completed,
                    &mut accounting,
                )?;
                if completed.is_some() {
                    break;
                }
            }
        }

        let summary = summary.finish().map_err(map_compaction_decode_error)?;
        let started = started.ok_or_else(protocol_sent)?;
        let completed = completed.unwrap_or_else(|| started.clone());
        ensure_sent_context(&context)?;
        yield ProviderEvent::canonical(GatewayEvent::Started(started));
        yield ProviderEvent::canonical(GatewayEvent::CompactionOutput(
            CompactionOutput::new(summary),
        ));
        for fact in accounting {
            yield ProviderEvent::canonical(fact);
        }
        yield ProviderEvent::canonical(GatewayEvent::Completed(completed));
    })
}

fn collect_compaction_facts(
    event: &ProviderEvent,
    started: &mut Option<ResponseMeta>,
    completed: &mut Option<ResponseMeta>,
    accounting: &mut Vec<GatewayEvent>,
) -> Result<(), ProviderError> {
    for fact in event.canonical_facts() {
        match fact {
            GatewayEvent::Started(meta) => {
                if started.is_none() {
                    *started = Some(meta.clone());
                }
            }
            GatewayEvent::Completed(meta) => {
                if completed.is_none() {
                    *completed = Some(meta.clone());
                }
            }
            GatewayEvent::Usage(_)
            | GatewayEvent::CalculatedCost(_)
            | GatewayEvent::ProviderCost(_) => accounting.push(fact.clone()),
            _ => {}
        }
    }
    Ok(())
}

fn map_compaction_decode_error(error: GrokCompactionDecodeError) -> ProviderError {
    match error {
        GrokCompactionDecodeError::Degenerate => mark_transient_compaction_failure(protocol_sent()),
        GrokCompactionDecodeError::InvalidSummary(_) => protocol_sent(),
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
        error.with_replay_safe().with_same_account_retry()
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
        instance,
        request,
        upstream_model,
        context,
        session,
        mut session_capture,
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
            request.turn_index(),
            &upstream_model,
        );
        let body = request.to_json_bytes().map_err(map_request_error)?;
        let inference_request = GrokInferenceRequest::new(
            instance.responses_url().clone(),
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
                let error =
                    map_continuation_failure(&context, map_transport_error_for_context(error, &context));
                let error = recover_or_record_failure(
                    selector.as_ref(),
                    credential_recovery.as_ref(),
                    &session,
                    error,
                    context.credential_recovery_attempted(),
                )
                .await;
                yield ProviderEvent::observation(observation);
                Err(error)?;
                return;
            }
        };

        let mut observation = ProviderResponseObservation::new(
            UpstreamTransport::new(HTTP_SSE_TRANSPORT).map_err(|_| provider_error(
                ProviderErrorKind::Protocol,
                UpstreamSendState::Sent,
            ))?,
        )
        .with_http_version(response.http_version())
        .with_status_code(response.status_code());
        if let Some(request_id) = response.request_id().cloned() {
            observation = observation.with_request_id(request_id);
        }
        yield ProviderEvent::observation(observation);

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
                    Some(Err(error)) => {
                        let error = map_stream_error(error);
                        Err(record_stream_failure(selector.as_ref(), &session, error).await)
                    },
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
                    let error = record_stream_failure(selector.as_ref(), &session, error).await;
                    Err(error)?;
                    return;
                }
            };
            let completed = events
                .iter()
                .flat_map(ProviderEvent::canonical_facts)
                .any(|event| matches!(event, GatewayEvent::Completed(_)));
            attach_xai_session_update(&mut events, &mut session_capture)?;
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
                let error = record_stream_failure(selector.as_ref(), &session, error).await;
                Err(error)?;
                return;
            }
        };
        attach_xai_session_update(&mut final_events, &mut session_capture)?;
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
    Ok(observation)
}

async fn record_failure(
    selector: &dyn GrokSessionSelector,
    session: &SelectedGrokSession,
    error: ProviderError,
) -> ProviderError {
    let failure = match error.kind() {
        ProviderErrorKind::Unauthorized => Some(GrokCredentialFailure::Unauthorized),
        ProviderErrorKind::RateLimited => Some(GrokCredentialFailure::RateLimited {
            retry_after: error.retry_after(),
        }),
        ProviderErrorKind::QuotaExhausted => Some(GrokCredentialFailure::QuotaExhausted),
        _ => None,
    };
    if let Some(failure) = failure {
        selector.record_failure(session, failure).await;
    }
    error
}

async fn record_stream_failure(
    selector: &dyn GrokSessionSelector,
    session: &SelectedGrokSession,
    error: ProviderError,
) -> ProviderError {
    if matches!(
        error.kind(),
        ProviderErrorKind::Unauthorized
            | ProviderErrorKind::RateLimited
            | ProviderErrorKind::QuotaExhausted
    ) {
        return record_failure(selector, session, error).await;
    }
    if error.kind() != ProviderErrorKind::Cancelled {
        selector
            .record_failure(session, GrokCredentialFailure::StreamInterrupted)
            .await;
    }
    error
}

async fn recover_or_record_failure(
    selector: &dyn GrokSessionSelector,
    recovery: &dyn GrokCredentialRecovery,
    session: &SelectedGrokSession,
    error: ProviderError,
    recovery_attempted: bool,
) -> ProviderError {
    if error.requires_credential_recovery() && !recovery_attempted {
        return match recovery
            .recover_unauthorized(session.account_id(), session.credential_revision())
            .await
        {
            GrokCredentialRecoveryOutcome::Recovered => error.with_same_account_retry(),
            GrokCredentialRecoveryOutcome::Rejected
            | GrokCredentialRecoveryOutcome::Unavailable => error,
        };
    }
    record_failure(selector, session, error).await
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
        | GrokRequestEncodeError::InvalidProviderOptions
        | GrokRequestEncodeError::InvalidRequestNormalization => ProviderErrorKind::InvalidRequest,
        GrokRequestEncodeError::UnsupportedProviderOption => ProviderErrorKind::Unsupported,
        GrokRequestEncodeError::Serialization => ProviderErrorKind::Protocol,
    };
    provider_error(kind, UpstreamSendState::NotSent)
}

fn map_instance_config_error(error: GrokProviderConfigError) -> ProviderError {
    let kind = match error {
        GrokProviderConfigError::ProviderMismatch => ProviderErrorKind::InvalidRequest,
        GrokProviderConfigError::InvalidBaseUrl | GrokProviderConfigError::UnsafeBaseUrl => {
            ProviderErrorKind::Protocol
        }
    };
    provider_error(kind, UpstreamSendState::NotSent)
}

fn map_selection_error(error: GrokSessionSelectorError) -> ProviderError {
    match error {
        GrokSessionSelectorError::CapacityUnavailable { retry_after } => {
            let error = provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent);
            match retry_after {
                Some(retry_after) => error.with_retry_after(retry_after),
                None => error,
            }
        }
        GrokSessionSelectorError::NoEligibleSession | GrokSessionSelectorError::Unavailable => {
            provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent)
        }
        GrokSessionSelectorError::InvalidSession => {
            provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
        }
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
    let kind = match error.kind() {
        GrokInferenceTransportErrorKind::InvalidRequest => ProviderErrorKind::InvalidRequest,
        GrokInferenceTransportErrorKind::Unsupported => ProviderErrorKind::Unsupported,
        GrokInferenceTransportErrorKind::Unauthorized => ProviderErrorKind::Unauthorized,
        GrokInferenceTransportErrorKind::PermissionDenied => ProviderErrorKind::PermissionDenied,
        GrokInferenceTransportErrorKind::RateLimited => ProviderErrorKind::RateLimited,
        GrokInferenceTransportErrorKind::QuotaExhausted => ProviderErrorKind::QuotaExhausted,
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
            && explicit_rejection_is_replay_safe(kind, status)
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
    if error.requires_credential_recovery() {
        mapped = mapped.with_credential_recovery().with_replay_safe();
    }
    if error.sensitive_context_was_redacted() {
        mapped = mapped.redact_sensitive_context("upstream transport context");
    }
    mapped
}

fn explicit_rejection_is_replay_safe(kind: ProviderErrorKind, status: u16) -> bool {
    matches!(
        (kind, status),
        (ProviderErrorKind::Unauthorized, 401)
            | (ProviderErrorKind::QuotaExhausted, 402)
            | (ProviderErrorKind::QuotaExhausted, 403)
            | (ProviderErrorKind::RateLimited, 429)
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
const CLI_RELEASE_WORKER_OWNER: &str = "xai-cli-release";

pub(crate) fn worker_contributions(
    refresh: Arc<GrokCredentialRefreshService>,
    quota: Arc<GrokCredentialQuotaService>,
    catalog: Arc<GrokCredentialCatalogService>,
    accounts: Arc<dyn ProviderAccountStore>,
    instances: Arc<dyn ProviderInstanceCatalogPort>,
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
                instances,
                quota,
                catalog,
                provider_kind,
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
            let outcomes = self
                .service
                .refresh_due()
                .await
                .map_err(|_| WorkerTaskError::safe("xAI OAuth refresh failed"))?;
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
            if failures > 0 {
                tracing::warn!(failures, "xAI OAuth refresh cycle contained failures");
            }
            Ok(())
        })
    }
}

struct XaiQuotaCatalogTask {
    accounts: Arc<dyn ProviderAccountStore>,
    instances: Arc<dyn ProviderInstanceCatalogPort>,
    quota: Arc<GrokCredentialQuotaService>,
    catalog: Arc<GrokCredentialCatalogService>,
    provider_kind: ProviderKind,
}

impl ScheduledTask for XaiQuotaCatalogTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let instances = self
                .instances
                .list_instances(&self.provider_kind, false)
                .await
                .map_err(|_| WorkerTaskError::safe("xAI Provider instances unavailable"))?;
            let mut failures = 0_u64;
            for config in instances {
                if context.cancellation().is_cancelled() {
                    return Ok(());
                }
                if !config.enabled() || config.provider_kind() != &self.provider_kind {
                    failures = failures.saturating_add(1);
                    continue;
                }
                let instance = ProviderInstance::new(
                    config.id().clone(),
                    config.provider_kind().clone(),
                    config.base_url().to_owned(),
                    true,
                    InstanceHealth::Healthy,
                );
                match self.accounts.list_for_instance(instance.id()).await {
                    Ok(accounts) => {
                        let now = SystemTime::now();
                        for account in accounts
                            .into_iter()
                            .filter(|account| eligible_quota_worker_account(account, now))
                        {
                            if context.cancellation().is_cancelled() {
                                return Ok(());
                            }
                            match self.quota.refresh_account(account.id()).await {
                                Ok(_) | Err(GrokQuotaError::AccountUnavailable) => {}
                                Err(_) => failures = failures.saturating_add(1),
                            }
                        }
                    }
                    Err(_) => failures = failures.saturating_add(1),
                }
                if self.catalog.query_instance_models(&instance).await.is_err() {
                    failures = failures.saturating_add(1);
                }
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

fn eligible_quota_worker_account(account: &ProviderAccount, now: SystemTime) -> bool {
    account.enabled()
        && account.access_token_expires_at() > now
        && match account.availability() {
            AccountAvailability::Unknown
            | AccountAvailability::Ready
            | AccountAvailability::QuotaExhausted => true,
            AccountAvailability::Cooldown => {
                account.cooldown_until().is_some_and(|until| until <= now)
            }
            AccountAvailability::Expired
            | AccountAvailability::Banned
            | AccountAvailability::Invalid => false,
        }
}
