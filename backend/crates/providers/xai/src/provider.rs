//! `gateway-core` Provider adapter for official Grok Build sessions.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures::{StreamExt, future::BoxFuture};
use gateway_core::engine::credential::{
    AccountAvailability, ProviderAccount, ProviderAccountStore,
};
use gateway_core::engine::provider::{
    EventStream, Provider, ProviderCallMetadata, ProviderCatalogGeneration,
    ProviderModelCapabilities, ProviderRequest, ProviderStream, UpstreamTransport,
};
use gateway_core::engine::{AttemptContext, UpstreamSendState};
use gateway_core::error::{ProviderError, ProviderErrorKind};
use gateway_core::event::{GatewayEvent, ProviderEvent, ProviderResponseObservation};
use gateway_core::operation::{Feature, Operation, OperationKind};
use gateway_core::provider_ports::ProviderInstanceCatalogPort;
use gateway_core::routing::{
    InstanceHealth, ModelCapabilities, ProviderInstance, ProviderKind, SupportLevel,
    UpstreamModelId,
};
use gateway_core::task::{
    ScheduledTask, WorkerContribution, WorkerCycleContext, WorkerDefinitionError, WorkerId,
    WorkerKind, WorkerLeaseRequest, WorkerRegistration, WorkerRunnable, WorkerSchedule,
    WorkerTaskError,
};

use crate::GrokCatalogCapabilityEvidence;
use crate::credential::{
    GrokCredentialCatalogService, GrokCredentialQuotaService, GrokCredentialRefreshOutcome,
    GrokCredentialRefreshService, GrokQuotaError,
};
use crate::transport::canonical::GrokCanonicalDecoder;
use crate::transport::config::XAI_PROVIDER_NAME;
use crate::transport::headers::build_grok_headers;
use crate::transport::{
    GrokCredentialFailure, GrokInferenceRequest, GrokInferenceTransport,
    GrokInferenceTransportError, GrokInferenceTransportErrorKind, GrokProviderConfigError,
    GrokProviderInstanceConfig, GrokRequestEncodeError, GrokResponsesRequest, GrokSessionSelection,
    GrokSessionSelector, GrokSessionSelectorError, SelectedGrokSession,
};

const HTTP_SSE_TRANSPORT: &str = "http_sse";

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
}

impl GrokBuildProvider {
    /// Creates a provider over explicit session and transport boundaries.
    #[must_use]
    pub fn new(
        selector: Arc<dyn GrokSessionSelector>,
        transport: Arc<dyn GrokInferenceTransport>,
        catalog: Arc<GrokCredentialCatalogService>,
    ) -> Self {
        Self {
            selector,
            transport,
            catalog,
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
                    support(model.capabilities().streaming_tool_calls()),
                )
                .with_feature(Feature::Vision, SupportLevel::Unknown)
                .with_feature(Feature::JsonSchema, SupportLevel::Unknown)
                .with_feature(Feature::NativeContinuation, SupportLevel::Unsupported);
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
        validate_continuation(&context)?;
        preflight_context(&context)?;

        let Operation::Generate(generate) = request.operation() else {
            return Err(provider_error(
                ProviderErrorKind::Unsupported,
                UpstreamSendState::NotSent,
            ));
        };
        let upstream_request =
            GrokResponsesRequest::encode(generate, candidate.upstream_model().as_str())
                .map_err(map_request_error)?;
        let selection = GrokSessionSelection::new(
            candidate.instance().clone(),
            candidate.upstream_model().clone(),
            context.excluded_accounts().clone(),
            context.required_account().cloned(),
            context.account_selection_policy(),
            context.deadline(),
        );
        let selection_deadline = remaining(context.deadline()).ok_or_else(|| {
            provider_error(ProviderErrorKind::Timeout, UpstreamSendState::NotSent)
        })?;
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
            selected = self.selector.select(selection) => selected.map_err(map_selection_error),
        }?;
        if context.excluded_accounts().contains(selected.account_id()) {
            return Err(provider_error(
                ProviderErrorKind::Protocol,
                UpstreamSendState::NotSent,
            ));
        }
        if context
            .required_account()
            .is_some_and(|required| required != selected.account_id())
        {
            return Err(provider_error(
                ProviderErrorKind::Protocol,
                UpstreamSendState::NotSent,
            ));
        }
        let selected = Arc::new(selected);
        let metadata = ProviderCallMetadata::new(
            ProviderKind::new(XAI_PROVIDER_NAME).map_err(|_| {
                provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
            })?,
            candidate.instance().clone(),
            candidate.upstream_model().clone(),
            selected.resource(),
            UpstreamTransport::new(HTTP_SSE_TRANSPORT).map_err(|_| {
                provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
            })?,
        );
        let events = cold_http_sse_stream(
            Arc::clone(&self.selector),
            Arc::clone(&self.transport),
            instance,
            upstream_request,
            candidate.upstream_model().clone(),
            context,
            Arc::clone(&selected),
        );
        Ok(ProviderStream::new(metadata, events, selected))
    }
}

fn support(evidence: GrokCatalogCapabilityEvidence) -> SupportLevel {
    match evidence {
        GrokCatalogCapabilityEvidence::DeclaredNative => SupportLevel::Native,
        GrokCatalogCapabilityEvidence::DeclaredUnsupported => SupportLevel::Unsupported,
        GrokCatalogCapabilityEvidence::Unknown => SupportLevel::Unknown,
    }
}

fn cold_http_sse_stream(
    selector: Arc<dyn GrokSessionSelector>,
    transport: Arc<dyn GrokInferenceTransport>,
    instance: GrokProviderInstanceConfig,
    request: GrokResponsesRequest,
    upstream_model: UpstreamModelId,
    context: AttemptContext,
    session: Arc<SelectedGrokSession>,
) -> EventStream {
    Box::pin(async_stream::try_stream! {
        if context.cancellation().is_cancelled() {
            Err(provider_error(
                ProviderErrorKind::Cancelled,
                UpstreamSendState::NotSent,
            ))?;
        }
        let headers = build_grok_headers(
            &instance,
            &session,
            context.request_id(),
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
                let error = record_failure(
                    selector.as_ref(),
                    &session,
                    map_transport_error(error),
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
        let mut decoder = GrokCanonicalDecoder::new(upstream_model.as_str());
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
                        Err(record_failure(selector.as_ref(), &session, error).await)
                    },
                    None => Ok(None),
                },
            }?;
            let Some(chunk) = next else {
                break;
            };
            let events = match decoder.push(&chunk) {
                Ok(events) => events,
                Err(error) => {
                    let error = record_failure(selector.as_ref(), &session, error).await;
                    Err(error)?;
                    return;
                }
            };
            let completed = events
                .iter()
                .flat_map(ProviderEvent::canonical_facts)
                .any(|event| matches!(event, GatewayEvent::Completed(_)));
            for event in events {
                ensure_sent_context(&context)?;
                yield event;
            }
            if completed {
                return;
            }
        }
        let final_events = match decoder.finish() {
            Ok(events) => events,
            Err(error) => {
                let error = record_failure(selector.as_ref(), &session, error).await;
                Err(error)?;
                return;
            }
        };
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

fn validate_continuation(context: &AttemptContext) -> Result<(), ProviderError> {
    if context.continuation().is_none() {
        return Ok(());
    }
    Err(provider_error(
        ProviderErrorKind::Unsupported,
        UpstreamSendState::NotSent,
    ))
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
        | GrokRequestEncodeError::InvalidProviderOptions => ProviderErrorKind::InvalidRequest,
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

fn map_transport_error(error: GrokInferenceTransportError) -> ProviderError {
    map_transport_error_with_state(error, None)
}

fn map_stream_error(error: GrokInferenceTransportError) -> ProviderError {
    map_transport_error_with_state(error, Some(UpstreamSendState::Sent))
}

fn map_transport_error_with_state(
    error: GrokInferenceTransportError,
    forced_send_state: Option<UpstreamSendState>,
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
        if forced_send_state.is_none() && explicit_rejection_is_replay_safe(kind, status) {
            mapped = mapped.with_replay_safe();
        }
    }
    if let Some(retry_after) = error.retry_after() {
        mapped = mapped.with_retry_after(retry_after);
    }
    if let Some(request_id) = error.request_id().cloned() {
        mapped = mapped.with_upstream_request_id(request_id);
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

pub(crate) fn worker_contributions(
    refresh: Arc<GrokCredentialRefreshService>,
    quota: Arc<GrokCredentialQuotaService>,
    catalog: Arc<GrokCredentialCatalogService>,
    accounts: Arc<dyn ProviderAccountStore>,
    instances: Arc<dyn ProviderInstanceCatalogPort>,
    provider_kind: ProviderKind,
) -> Result<Vec<WorkerContribution>, WorkerDefinitionError> {
    let refresh_id = WorkerId::try_new(WorkerKind::OAuthRefresh, XAI_PROVIDER_NAME)?;
    let catalog_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, XAI_PROVIDER_NAME)?;
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
