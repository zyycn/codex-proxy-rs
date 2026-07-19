//! Codex 的 `gateway-core` Provider adapter。

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures::StreamExt;
use gateway_core::engine::provider::{
    EventStream, Provider, ProviderCallMetadata, ProviderModelCapabilities, ProviderRequest,
    ProviderResource, ProviderStream, UpstreamTransport,
};
use gateway_core::engine::{AttemptContext, UpstreamSendState};
use gateway_core::error::{ProviderError, ProviderErrorKind};
use gateway_core::event::GatewayEvent;
use gateway_core::operation::{Feature, Operation, OperationKind};
use gateway_core::routing::{
    ModelCapabilities, ProviderInstance, ProviderInstanceId, ProviderKind, SupportLevel,
    UpstreamModelId,
};
use reqwest::{Client, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use url::{Host, Url};

use crate::credential::{
    CodexCredentialCatalogService, CodexCredentialLease, CodexCredentialSelector,
    CredentialSelectionError, RuntimeCodexCookie, SelectCodexCredential,
};
use crate::transport::canonical::CodexCanonicalDecoder;
use crate::transport::catalog::{CodexCatalogCapabilityEvidence, CodexCatalogModel};
use crate::transport::profile::CodexWireProfileState;
use crate::transport::protocol::responses::{CodexResponsesRequest, PreviousResponseScope};
use crate::transport::request::{CodexRequestEncodeError, encode_generate_request};
use crate::transport::{
    CODEX_RESPONSES_PATH, CodexBackendClient, CodexBackendTransport, CodexClientError,
    CodexRequestContext, CodexWebSocketPool, build_reqwest_client, endpoint_url,
};

const PROVIDER_NAME: &str = "openai";
const HTTP_SSE_TRANSPORT: &str = "http_sse";
const WEBSOCKET_TRANSPORT: &str = "websocket";
const MAX_COOKIE_HEADER_BYTES: usize = 16 * 1024;
const OFFICIAL_CODEX_HOST: &str = "chatgpt.com";
pub const OFFICIAL_CODEX_BASE_PATH: &str = "/backend-api";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexProviderTransport {
    HttpSse,
    WebSocket,
}

impl CodexProviderTransport {
    fn parse(value: &str) -> Option<Self> {
        match value {
            HTTP_SSE_TRANSPORT => Some(Self::HttpSse),
            WEBSOCKET_TRANSPORT => Some(Self::WebSocket),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexEndpointPolicy {
    Official,
    Loopback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexProviderInstanceConfig {
    id: ProviderInstanceId,
    base_url: Url,
    transport: CodexProviderTransport,
}

impl CodexProviderInstanceConfig {
    pub fn from_snapshot(instance: &ProviderInstance) -> Result<Self, CodexProviderConfigError> {
        Self::from_snapshot_with_policy(instance, CodexEndpointPolicy::Official)
    }

    pub fn from_snapshot_with_policy(
        instance: &ProviderInstance,
        endpoint_policy: CodexEndpointPolicy,
    ) -> Result<Self, CodexProviderConfigError> {
        if instance.provider().as_str() != PROVIDER_NAME {
            return Err(CodexProviderConfigError::ProviderMismatch);
        }
        let mut base_url = Url::parse(instance.base_url())
            .map_err(|_| CodexProviderConfigError::InvalidBaseUrl)?;
        normalize_and_validate_base_url(&mut base_url, endpoint_policy)?;
        Ok(Self {
            id: instance.id().clone(),
            base_url,
            transport: CodexProviderTransport::HttpSse,
        })
    }

    #[must_use]
    pub const fn id(&self) -> &ProviderInstanceId {
        &self.id
    }

    #[must_use]
    pub const fn base_url(&self) -> &Url {
        &self.base_url
    }

    #[must_use]
    pub const fn transport(&self) -> CodexProviderTransport {
        self.transport
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodexProviderConfigError {
    #[error("provider instance does not belong to Codex")]
    ProviderMismatch,
    #[error("Codex provider base URL is invalid")]
    InvalidBaseUrl,
    #[error("Codex provider base URL is not allowed")]
    UnsafeBaseUrl,
    #[error("Codex provider HTTP client initialization failed")]
    TransportInitialization,
}

#[derive(Clone)]
struct CompiledInstance {
    config: CodexProviderInstanceConfig,
    responses_url: Url,
    client: CodexBackendClient,
}

pub struct CodexProvider {
    selector: Arc<CodexCredentialSelector>,
    catalog: Arc<CodexCredentialCatalogService>,
    http: Client,
    profile: CodexWireProfileState,
    endpoint_policy: CodexEndpointPolicy,
    websocket_pool: Arc<CodexWebSocketPool>,
}

impl CodexProvider {
    pub fn new(
        selector: Arc<CodexCredentialSelector>,
        catalog: Arc<CodexCredentialCatalogService>,
        profile: CodexWireProfileState,
    ) -> Result<Self, CodexProviderConfigError> {
        let http = build_reqwest_client()
            .map_err(|_| CodexProviderConfigError::TransportInitialization)?;
        Ok(Self {
            selector,
            catalog,
            http,
            profile,
            endpoint_policy: CodexEndpointPolicy::Official,
            websocket_pool: Arc::new(CodexWebSocketPool::default()),
        })
    }

    pub fn new_with_policy(
        selector: Arc<CodexCredentialSelector>,
        catalog: Arc<CodexCredentialCatalogService>,
        profile: CodexWireProfileState,
        endpoint_policy: CodexEndpointPolicy,
    ) -> Result<Self, CodexProviderConfigError> {
        let mut provider = Self::new(selector, catalog, profile)?;
        provider.endpoint_policy = endpoint_policy;
        Ok(provider)
    }

    pub fn validate_instance(
        instance: &ProviderInstance,
    ) -> Result<CodexProviderInstanceConfig, CodexProviderConfigError> {
        CodexProviderInstanceConfig::from_snapshot(instance)
    }

    fn compile_instance(
        &self,
        snapshot: &ProviderInstance,
    ) -> Result<CompiledInstance, ProviderError> {
        let config =
            CodexProviderInstanceConfig::from_snapshot_with_policy(snapshot, self.endpoint_policy)
                .map_err(map_instance_config_error)?;
        let responses_url = Url::parse(&endpoint_url(
            config.base_url.as_str(),
            CODEX_RESPONSES_PATH,
        ))
        .map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent))?;
        Ok(CompiledInstance {
            client: CodexBackendClient::new(
                self.http.clone(),
                config.base_url.as_str(),
                self.profile.clone(),
            )
            .with_websocket_pool(Arc::clone(&self.websocket_pool)),
            config,
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
            .field("endpoint_policy", &self.endpoint_policy)
            .finish()
    }
}

#[async_trait]
impl Provider for CodexProvider {
    fn name(&self) -> &'static str {
        PROVIDER_NAME
    }

    async fn query_model_capabilities(
        &self,
        instance: &ProviderInstance,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        let snapshot = self
            .catalog
            .synchronize_instance(instance)
            .await
            .map_err(|_| {
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
        let instance_snapshot = candidate.instance_snapshot();
        if instance_snapshot.id() != candidate.instance() {
            return Err(provider_error(
                ProviderErrorKind::Protocol,
                UpstreamSendState::NotSent,
            ));
        }
        let instance = self.compile_instance(instance_snapshot)?;
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
        let mut upstream_request =
            encode_generate_request(generate, candidate.upstream_model().as_str())
                .map_err(map_request_error)?;
        let transport =
            selected_transport(&request, &instance.config, context.continuation().is_some())?;
        if let Some(continuation) = context.continuation() {
            upstream_request.set_previous_response_id(Some(
                continuation.upstream_response_id().as_str().to_owned(),
            ));
            // Codex 请求统一 store=false；原生 handle 只能在仍持有它的连接上续接。
            upstream_request.previous_response_scope = Some(PreviousResponseScope::ConnectionLocal);
        }
        apply_transport(&mut upstream_request, transport);

        let lease = self
            .selector
            .select(&SelectCodexCredential {
                provider_instance_id: candidate.instance(),
                request_url: &instance.responses_url,
                attempt: &context,
            })
            .await
            .map_err(map_selection_error)?;
        let lease = Arc::new(lease);
        let metadata = ProviderCallMetadata::new(
            ProviderKind::new(PROVIDER_NAME).map_err(|_| {
                provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
            })?,
            candidate.instance().clone(),
            candidate.upstream_model().clone(),
            ProviderResource::Account {
                id: lease.account_id().clone(),
                revision: lease.account().revision(),
            },
            UpstreamTransport::new(transport_name(transport)).map_err(|_| {
                provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
            })?,
        );
        let events = cold_response_stream(ColdResponse {
            client: instance.client,
            response_origin: instance.responses_url,
            request: upstream_request,
            upstream_model: candidate.upstream_model().clone(),
            expected_transport: transport,
            context,
            selector: Arc::clone(&self.selector),
            lease: Arc::clone(&lease),
        });
        Ok(ProviderStream::new(metadata, events, lease))
    }
}

struct ColdResponse {
    client: CodexBackendClient,
    response_origin: Url,
    request: CodexResponsesRequest,
    upstream_model: UpstreamModelId,
    expected_transport: CodexProviderTransport,
    context: AttemptContext,
    selector: Arc<CodexCredentialSelector>,
    lease: Arc<CodexCredentialLease>,
}

fn cold_response_stream(response: ColdResponse) -> EventStream {
    let ColdResponse {
        client,
        response_origin,
        request,
        upstream_model,
        expected_transport,
        context,
        selector,
        lease,
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
            _ = cancellation.cancelled() => Err(provider_error(
                ProviderErrorKind::Cancelled,
                UpstreamSendState::Ambiguous,
            )),
            _ = tokio::time::sleep(handshake_deadline) => Err(provider_error(
                ProviderErrorKind::Timeout,
                UpstreamSendState::Ambiguous,
            )),
            response = client.create_response_stream_with_pool_account(
                &request,
                request_context,
                Some(lease.account_id().as_str()),
            ) => response.map_err(map_handshake_error),
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                selector
                    .record_failure(&lease, error.kind(), error.send_state(), error.retry_after())
                    .await;
                Err(error)?;
                return;
            }
        };
        if response.transport != backend_transport(expected_transport) {
            Err(provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent))?;
        }
        if !response.set_cookie_headers.is_empty() {
            let _ = selector
                .capture_response_cookies(&lease, &response_origin, &response.set_cookie_headers)
                .await;
        }
        let mut body = response.body;
        let mut decoder = CodexCanonicalDecoder::new(upstream_model.as_str());
        loop {
            let Some(stream_deadline) = remaining(context.deadline()) else {
                Err(provider_error(ProviderErrorKind::Timeout, UpstreamSendState::Sent))?;
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
                    Some(Err(error)) => Err(map_stream_error(error)),
                    None => Ok(None),
                },
            };
            let next = match next {
                Ok(next) => next,
                Err(error) => {
                    selector
                        .record_failure(&lease, error.kind(), error.send_state(), error.retry_after())
                        .await;
                    Err(error)?;
                    return;
                }
            };
            let Some(chunk) = next else { break; };
            let events = decoder.push(&chunk)?;
            let completed = events
                .iter()
                .any(|event| matches!(event, GatewayEvent::Completed(_)));
            for event in events {
                yield event;
            }
            if completed {
                return;
            }
        }
        for event in decoder.finish()? {
            yield event;
        }
    })
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
    let mut capabilities = ModelCapabilities::new(operations, context_window, None);
    capabilities = capabilities.with_feature(
        Feature::NativeContinuation,
        support(evidence.responses_api()),
    );
    capabilities = capabilities.with_feature(Feature::Reasoning, support(evidence.reasoning()));
    capabilities =
        capabilities.with_feature(Feature::Tools, support(evidence.parallel_tool_calls()));
    capabilities = capabilities.with_feature(Feature::Vision, support(evidence.image_input()));
    ProviderModelCapabilities::new(model.request_model().clone(), capabilities)
}

const fn support(evidence: CodexCatalogCapabilityEvidence) -> SupportLevel {
    match evidence {
        CodexCatalogCapabilityEvidence::DeclaredNative => SupportLevel::Native,
        CodexCatalogCapabilityEvidence::DeclaredUnsupported => SupportLevel::Unsupported,
        CodexCatalogCapabilityEvidence::Unknown => SupportLevel::Unknown,
    }
}

fn selected_transport(
    request: &ProviderRequest,
    instance: &CodexProviderInstanceConfig,
    has_continuation: bool,
) -> Result<CodexProviderTransport, ProviderError> {
    if has_continuation {
        return Ok(CodexProviderTransport::WebSocket);
    }
    let mut transport = instance.transport();
    if let Some(value) = request
        .operation()
        .provider_options(PROVIDER_NAME)
        .and_then(|options| options.get("transport"))
    {
        transport = value
            .as_str()
            .and_then(CodexProviderTransport::parse)
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
    let websocket = transport == CodexProviderTransport::WebSocket;
    request.force_http_sse = !websocket;
    request.force_websocket = websocket;
    request.use_websocket = websocket;
}

const fn transport_name(transport: CodexProviderTransport) -> &'static str {
    match transport {
        CodexProviderTransport::HttpSse => HTTP_SSE_TRANSPORT,
        CodexProviderTransport::WebSocket => WEBSOCKET_TRANSPORT,
    }
}

const fn backend_transport(transport: CodexProviderTransport) -> CodexBackendTransport {
    match transport {
        CodexProviderTransport::HttpSse => CodexBackendTransport::HttpSse,
        CodexProviderTransport::WebSocket => CodexBackendTransport::WebSocket,
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

fn map_instance_config_error(error: CodexProviderConfigError) -> ProviderError {
    let kind = match error {
        CodexProviderConfigError::ProviderMismatch => ProviderErrorKind::InvalidRequest,
        CodexProviderConfigError::InvalidBaseUrl
        | CodexProviderConfigError::UnsafeBaseUrl
        | CodexProviderConfigError::TransportInitialization => ProviderErrorKind::Protocol,
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

fn map_handshake_error(error: CodexClientError) -> ProviderError {
    map_client_error(error, UpstreamSendState::Ambiguous)
}

fn map_stream_error(error: CodexClientError) -> ProviderError {
    map_client_error(error, UpstreamSendState::Sent)
}

fn map_client_error(error: CodexClientError, uncertain_state: UpstreamSendState) -> ProviderError {
    match error {
        CodexClientError::Upstream {
            status,
            retry_after_seconds,
            ..
        } => {
            let kind = http_error_kind(status);
            let mut mapped = provider_error(kind, UpstreamSendState::Sent)
                .with_status(status.as_u16())
                .redact_sensitive_context("upstream response body");
            if explicit_rejection_is_replay_safe(kind, status.as_u16()) {
                mapped = mapped.with_replay_safe();
            }
            match retry_after_seconds {
                Some(seconds) => mapped.with_retry_after(Duration::from_secs(seconds)),
                None => mapped,
            }
        }
        CodexClientError::InvalidHeaderName(_)
        | CodexClientError::InvalidHeaderValue(_)
        | CodexClientError::WebSocketEncode(_)
        | CodexClientError::ModelCatalog(_)
        | CodexClientError::CustomCa(_) => {
            provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
        }
        CodexClientError::StreamIdleTimeout { .. } => {
            provider_error(ProviderErrorKind::Timeout, UpstreamSendState::Sent)
        }
        CodexClientError::InvalidSse(_) => {
            provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
        }
        CodexClientError::Http(error) => provider_error(
            if error.is_timeout() {
                ProviderErrorKind::Timeout
            } else {
                ProviderErrorKind::Transport
            },
            uncertain_state,
        ),
        CodexClientError::WebSocket(_) => {
            provider_error(ProviderErrorKind::Unavailable, uncertain_state)
        }
    }
}

fn http_error_kind(status: StatusCode) -> ProviderErrorKind {
    match status.as_u16() {
        400 | 404 | 409 | 422 => ProviderErrorKind::InvalidRequest,
        401 => ProviderErrorKind::Unauthorized,
        402 => ProviderErrorKind::QuotaExhausted,
        403 => ProviderErrorKind::PermissionDenied,
        408 | 504 => ProviderErrorKind::Timeout,
        429 => ProviderErrorKind::RateLimited,
        500..=599 => ProviderErrorKind::Unavailable,
        _ => ProviderErrorKind::Transport,
    }
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

fn normalize_and_validate_base_url(
    url: &mut Url,
    endpoint_policy: CodexEndpointPolicy,
) -> Result<(), CodexProviderConfigError> {
    if url.cannot_be_a_base()
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(CodexProviderConfigError::UnsafeBaseUrl);
    }
    let allowed = match endpoint_policy {
        CodexEndpointPolicy::Official => {
            url.scheme() == "https"
                && matches!(url.host(), Some(Host::Domain(host)) if host == OFFICIAL_CODEX_HOST)
                && url.port().is_none()
        }
        CodexEndpointPolicy::Loopback => {
            url.scheme() == "http"
                && matches!(url.host(), Some(Host::Ipv4(host)) if host.is_loopback())
        }
    };
    if !allowed {
        return Err(CodexProviderConfigError::UnsafeBaseUrl);
    }
    let normalized_path = url.path().trim_end_matches('/');
    if normalized_path != OFFICIAL_CODEX_BASE_PATH {
        return Err(CodexProviderConfigError::UnsafeBaseUrl);
    }
    url.set_path(OFFICIAL_CODEX_BASE_PATH);
    Ok(())
}
