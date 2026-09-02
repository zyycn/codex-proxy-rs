//! OpenAI 请求、响应与 transport 观测事实归一化。

use super::*;

/// OpenAI Responses 的观测状态完全归 Provider 所有。
///
/// 每次原始 SSE/WS 事件推进状态后重新生成不可变 observation，Core 只负责携带、
/// 持久化和展示这份安全快照，不解释任何 OpenAI 协议字段。
pub(super) struct OpenAiResponseObservationState {
    transport: CodexBackendTransport,
    diagnostics: CodexUpstreamDiagnostics,
    response_metadata: CodexResponseMetadata,
    metrics: CodexTransportMetrics,
    websocket_pool_decision: Option<crate::transport::WebSocketPoolDecision>,
    request_summary: Value,
    requested_model: String,
    stream: bool,
    compact: bool,
    requested_service_tier: Option<String>,
    upstream_service_tier: Option<String>,
    rate_limit_headers: Vec<(String, String)>,
    account_id: String,
    attempt_index: u32,
    timings: ProviderResponseTimings,
    terminal: Option<OpenAiResponseTerminal>,
}

pub(super) struct OpenAiPassiveQuotaObservation {
    rate_limits: Vec<ParsedRateLimits>,
}

impl OpenAiPassiveQuotaObservation {
    pub(super) fn new(headers: Vec<(String, String)>) -> Self {
        Self {
            rate_limits: parse_rate_limit_headers(&headers).into_iter().collect(),
        }
    }

    pub(super) fn observe(&mut self, updates: &[ParsedRateLimits]) {
        self.rate_limits.extend_from_slice(updates);
    }

    pub(super) fn rate_limits(&self) -> &[ParsedRateLimits] {
        &self.rate_limits
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OpenAiResponseTerminal {
    Completed,
    Incomplete,
}

impl OpenAiResponseObservationState {
    pub(super) fn from_backend_response(
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
            requested_service_tier: normalize_service_tier(request.service_tier()),
            upstream_service_tier: None,
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

    pub(super) fn observation(&self) -> Option<ProviderResponseObservation> {
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
        if let Some(service_tier) = self.effective_service_tier() {
            observation = observation.with_service_tier_if_valid(service_tier.to_owned());
        }
        Some(observation)
    }

    pub(super) fn observe_stream_chunk(&mut self, chunk: &[u8], started_at: Instant) -> bool {
        if chunk.is_empty() {
            return false;
        }
        insert_first_timing(&mut self.timings.first_event_ms, started_at)
    }

    pub(super) fn observe_timing_signals(
        &mut self,
        signals: ResponseEventSignals,
        started_at: Instant,
    ) -> bool {
        let mut changed = false;
        // 首个非前导输出事件（结构帧也算）开启首字计时；
        // 真实语义首字由 first_reasoning_ms / first_text_ms 单独观测。
        if signals.output_start {
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

    pub(super) fn observe_upstream_service_tier(&mut self, service_tier: Option<&str>) -> bool {
        let Some(service_tier) = normalize_service_tier(service_tier) else {
            return false;
        };
        if self.upstream_service_tier.as_deref() == Some(service_tier.as_str()) {
            return false;
        }
        self.upstream_service_tier = Some(service_tier);
        true
    }

    pub(super) fn effective_service_tier(&self) -> Option<&str> {
        self.requested_service_tier
            .as_deref()
            .or(self.upstream_service_tier.as_deref())
    }

    pub(super) fn merge_rate_limit_headers(&mut self, updates: &[(String, String)]) -> bool {
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

    pub(super) fn merge_client_header(&mut self, name: &str, value: &str) -> bool {
        let value = Bytes::copy_from_slice(value.as_bytes());
        if let Some((_, existing)) = self
            .response_metadata
            .client_headers
            .iter_mut()
            .find(|(existing_name, _)| existing_name.eq_ignore_ascii_case(name))
        {
            if *existing == value {
                return false;
            }
            *existing = value;
            return true;
        }
        self.response_metadata
            .client_headers
            .push((name.to_owned(), value));
        true
    }

    pub(super) fn mark_completed(&mut self, incomplete: bool) -> bool {
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

    pub(super) fn provider_metadata(&self) -> Option<ProviderResponseMetadata> {
        let mut metadata = Map::new();
        // 观测对象版本信封：读取端按 schemaVersion 解码，缺失视为 v0。
        metadata.insert("schemaVersion".to_owned(), json!(1));
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
        if let Some(service_tier) = self.effective_service_tier() {
            metadata.insert(
                "serviceTier".to_owned(),
                Value::String(service_tier.to_owned()),
            );
        }
        if let Some(service_tier) = &self.requested_service_tier {
            metadata.insert(
                "requestedServiceTier".to_owned(),
                Value::String(service_tier.clone()),
            );
        }
        if let Some(service_tier) = &self.upstream_service_tier {
            metadata.insert(
                "upstreamServiceTier".to_owned(),
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

pub(super) fn codex_response_observation(
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
    // WebSocket opening 的 101 是成功升级事实，不是业务请求的失败 HTTP 状态。
    // opening 明确拒绝仍由 `codex_error_observation` 保存真实上游状态。
    if transport != CodexBackendTransport::WebSocket
        && let Some(status_code) = diagnostics.status_code
    {
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

pub(super) fn codex_error_observation(
    error: &CodexClientError,
) -> Option<ProviderResponseObservation> {
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
        CodexClientError::WebSocket(error)
            if matches!(error.classified(), CodexWebSocketExchangeError::Upstream(_)) =>
        {
            let CodexWebSocketExchangeError::Upstream(upstream) = error.classified() else {
                unreachable!("websocket error was checked above")
            };
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

pub(super) fn openai_response_timings(
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

pub(super) fn openai_processing_ms(response_metadata: &CodexResponseMetadata) -> Option<u64> {
    response_metadata
        .client_headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("openai-processing-ms"))
        .and_then(|(_, value)| std::str::from_utf8(value).ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
}

pub(super) fn openai_response_request_summary(
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

pub(super) const fn json_value_kind(value: Option<&Value>) -> &'static str {
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

pub(super) fn selected_observation_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| selected_observation_header(name, value))
        .collect()
}

pub(super) fn selected_observation_header(name: &str, value: &str) -> Option<(String, String)> {
    let name = name.trim().to_ascii_lowercase();
    (!name.is_empty()).then(|| (name, value.to_owned()))
}

pub(super) fn insert_first_timing(target: &mut Option<u64>, started_at: Instant) -> bool {
    if target.is_some() {
        return false;
    }
    let elapsed = u64::try_from(started_at.elapsed().as_millis())
        .unwrap_or(u64::MAX)
        .max(1);
    *target = Some(elapsed);
    true
}

pub(super) fn insert_optional_metadata_millis(
    metadata: &mut Map<String, Value>,
    name: &str,
    value: Option<u64>,
) {
    if let Some(value) = value {
        metadata.insert(name.to_owned(), json!(value));
    }
}

pub(super) async fn take_rate_limit_updates(
    updates: Option<&CodexRateLimitUpdates>,
) -> Vec<ParsedRateLimits> {
    let Some(updates) = updates else {
        return Vec::new();
    };
    std::mem::take(&mut *updates.lock().await)
}

pub(super) fn rate_limit_update_headers(updates: &[ParsedRateLimits]) -> Vec<(String, String)> {
    updates
        .iter()
        .flat_map(rate_limits_to_header_pairs)
        .collect()
}

pub(super) fn terminal_response_is_incomplete(events: &[ProviderEvent]) -> bool {
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

pub(super) async fn synchronize_passive_quota(
    quota: &CodexCredentialQuotaService,
    account: &ProviderAccount,
    rate_limits: &[ParsedRateLimits],
) {
    if rate_limits.is_empty() {
        return;
    }
    if let Err(error) = quota
        .synchronize_passive_rate_limits(account, rate_limits)
        .await
    {
        tracing::warn!(
            account_id = %account.id(),
            error = %error,
            "OpenAI passive quota synchronization failed"
        );
    }
}

pub(super) async fn synchronize_passive_quota_headers(
    quota: &CodexCredentialQuotaService,
    account: &ProviderAccount,
    headers: &[(String, String)],
) {
    let Some(rate_limits) = parse_rate_limit_headers(headers) else {
        return;
    };
    synchronize_passive_quota(quota, account, std::slice::from_ref(&rate_limits)).await;
}

pub(super) const fn actual_transport_name(transport: CodexBackendTransport) -> &'static str {
    match transport {
        CodexBackendTransport::HttpSse => HTTP_SSE_TRANSPORT,
        CodexBackendTransport::HttpJson => HTTP_JSON_TRANSPORT,
        CodexBackendTransport::WebSocket => WEBSOCKET_TRANSPORT,
    }
}

pub(super) fn nonnegative_millis(value: Option<i64>) -> Option<u64> {
    value.and_then(|value| u64::try_from(value).ok())
}

pub(super) fn compile_model_capabilities(model: &CodexCatalogModel) -> ProviderModelCapabilities {
    // Catalog membership is enough to publish Generate. `supported_in_api` is advisory;
    // the upstream response is authoritative for normal and diagnostic requests.
    let capabilities = ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), None)
        .with_upstream_feature_validation();
    ProviderModelCapabilities::new(model.request_model().clone(), capabilities)
        .with_presentation(codex_model_presentation(model))
}

pub(super) fn codex_model_presentation(model: &CodexCatalogModel) -> ModelPresentation {
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

pub(super) fn selected_transport(request: &CodexResponsesRequest) -> CodexProviderTransport {
    if request.force_http_sse {
        CodexProviderTransport::HttpOnly
    } else {
        CodexProviderTransport::PreferWebSocket
    }
}

pub(super) fn apply_transport(
    request: &mut CodexResponsesRequest,
    transport: CodexProviderTransport,
) {
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

pub(super) const fn transport_name(transport: CodexProviderTransport) -> &'static str {
    match transport {
        CodexProviderTransport::HttpOnly => HTTP_SSE_TRANSPORT,
        CodexProviderTransport::PreferWebSocket => WEBSOCKET_TRANSPORT,
    }
}

pub(super) const fn accepts_backend_transport(
    transport: CodexProviderTransport,
    actual: CodexBackendTransport,
) -> bool {
    match transport {
        CodexProviderTransport::HttpOnly => matches!(actual, CodexBackendTransport::HttpSse),
        CodexProviderTransport::PreferWebSocket => true,
    }
}

pub(super) fn codex_request_context<'a>(
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

pub(super) fn build_cookie_header(
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

pub(super) fn valid_cookie_name(name: &str) -> bool {
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

pub(super) fn map_request_error(error: CodexRequestEncodeError) -> ProviderError {
    let kind = match error {
        CodexRequestEncodeError::InvalidProtocolPayload => ProviderErrorKind::InvalidRequest,
    };
    provider_error(kind, UpstreamSendState::NotSent)
}

pub(super) fn map_selection_error(error: CredentialSelectionError) -> ProviderError {
    match error {
        CredentialSelectionError::CapacityUnavailable { retry_after } => {
            let error = provider_error(
                ProviderErrorKind::AccountCapacityUnavailable,
                UpstreamSendState::NotSent,
            );
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
        | CredentialSelectionError::CookiePolicy => provider_error(
            ProviderErrorKind::ProviderInfrastructureUnavailable,
            UpstreamSendState::NotSent,
        ),
    }
}
