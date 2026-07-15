use super::*;

impl CodexBackendClient {
    /// 构造客户端。
    pub fn new(
        client: Client,
        base_url: impl Into<String>,
        fingerprint: RuntimeFingerprint,
    ) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        Self {
            client,
            websocket_origin_key: websocket_origin_key(&base_url),
            base_url,
            fingerprint,
            websocket_pool: None,
            websocket_initial_event_timeout: Some(DEFAULT_INITIAL_EVENT_TIMEOUT),
            websocket_fast_path_budget: WEBSOCKET_FAST_PATH_BUDGET,
            websocket_origin_breaker: WebSocketOriginBreaker::default(),
        }
    }

    pub fn with_websocket_initial_event_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.websocket_initial_event_timeout = timeout.filter(|timeout| !timeout.is_zero());
        self
    }

    pub fn with_websocket_fast_path_budget(mut self, budget: Duration) -> Self {
        self.websocket_fast_path_budget = budget.max(Duration::from_millis(1));
        self
    }

    pub fn with_websocket_origin_breaker(mut self, breaker: WebSocketOriginBreaker) -> Self {
        self.websocket_origin_breaker = breaker;
        self
    }

    /// 为 Responses WebSocket 请求启用连接池。
    pub fn with_websocket_pool(mut self, pool: Arc<CodexWebSocketPool>) -> Self {
        self.websocket_pool = Some(pool);
        self
    }

    /// 驱逐指定账号的 Responses WebSocket 池连接。
    pub async fn evict_websocket_account(&self, account_id: &str) {
        if let Some(pool) = &self.websocket_pool {
            pool.evict_account(account_id).await;
        }
    }

    /// 发送 Responses SSE 请求并读取完整响应。
    /// HTTP POST + SSE fallback (when WebSocket pool is disabled).
    async fn create_response_http_sse(
        &self,
        upstream_request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        started_at: Instant,
    ) -> CodexClientResult<CodexBackendResponse> {
        let headers = self.request_headers_for_http_response(upstream_request, context)?;
        let headers_started_at = Instant::now();
        let response = self
            .client
            .post(endpoint_url(&self.base_url, CODEX_RESPONSES_PATH))
            .headers(headers)
            .json(&upstream_request)
            .send()
            .await?;
        let upstream_headers_ms = elapsed_duration_millis(headers_started_at.elapsed());
        let http_version = http_version_name(response.version()).to_string();
        let status = response.status();
        let diagnostics = response_meta::diagnostics(Some(status.as_u16()), response.headers());
        let turn_state = response_meta::turn_state(response.headers());
        let set_cookie_headers = response_meta::set_cookie_headers(response.headers());
        let rate_limit_headers = response_meta::rate_limit_headers(response.headers());
        let response_metadata = response_meta::response_metadata(response.headers());
        let retry_after_seconds = retry_after_seconds(response.headers(), None);

        if !status.is_success() {
            let body = read_capped_error_body(response).await?;
            return Err(CodexClientError::Upstream {
                status,
                retry_after_seconds: retry_after_seconds
                    .or_else(|| retry_after_seconds_from_body(&body)),
                body,
                diagnostics,
                set_cookie_headers,
                transport: CodexBackendTransport::HttpSse,
            });
        }

        let mut body_bytes = Vec::new();
        let mut first_token_ms = None;
        let mut first_event_ms = None;
        let mut stream = http_sse_stream(response);
        while let Some(chunk) = stream.try_next().await? {
            first_event_ms.get_or_insert_with(|| elapsed_duration_millis(started_at.elapsed()));
            body_bytes.extend_from_slice(&chunk);
            response_meta::update_first_token_ms(started_at, &body_bytes, &mut first_token_ms);
        }
        let body = String::from_utf8_lossy(&body_bytes).into_owned();
        let usage = extract_sse_usage(&body).map_err(CodexClientError::InvalidSse)?;
        Ok(CodexBackendResponse {
            body,
            transport: CodexBackendTransport::HttpSse,
            usage,
            turn_state,
            set_cookie_headers,
            rate_limit_headers,
            first_token_ms,
            websocket_pool_decision: None,
            diagnostics,
            response_metadata,
            transport_metrics: CodexTransportMetrics {
                upstream_headers_ms: Some(upstream_headers_ms),
                first_event_ms,
                http_version: Some(http_version),
                ..CodexTransportMetrics::default()
            },
            connection_local_continuation: false,
        })
    }

    /// 发送 Responses SSE 请求并返回 live SSE 流（HTTP SSE fallback）。
    async fn create_response_stream_http_sse(
        &self,
        upstream_request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendStreamingResponse> {
        let headers = self.request_headers_for_http_response(upstream_request, context)?;
        let headers_started_at = Instant::now();
        let response = self
            .client
            .post(endpoint_url(&self.base_url, CODEX_RESPONSES_PATH))
            .headers(headers)
            .json(&upstream_request)
            .send()
            .await?;
        let upstream_headers_ms = elapsed_duration_millis(headers_started_at.elapsed());
        let http_version = http_version_name(response.version()).to_string();
        let status = response.status();
        let diagnostics = response_meta::diagnostics(Some(status.as_u16()), response.headers());
        let turn_state = response_meta::turn_state(response.headers());
        let set_cookie_headers = response_meta::set_cookie_headers(response.headers());
        let rate_limit_headers = response_meta::rate_limit_headers(response.headers());
        let response_metadata = response_meta::response_metadata(response.headers());
        let retry_after_seconds = retry_after_seconds(response.headers(), None);

        if !status.is_success() {
            let body = read_capped_error_body(response).await?;
            return Err(CodexClientError::Upstream {
                status,
                retry_after_seconds: retry_after_seconds
                    .or_else(|| retry_after_seconds_from_body(&body)),
                body,
                diagnostics,
                set_cookie_headers,
                transport: CodexBackendTransport::HttpSse,
            });
        }

        Ok(CodexBackendStreamingResponse {
            body: http_sse_stream(response),
            transport: CodexBackendTransport::HttpSse,
            turn_state,
            set_cookie_headers,
            rate_limit_headers,
            rate_limit_header_updates: None,
            turn_state_update: None,
            websocket_pool_decision: None,
            diagnostics,
            response_metadata,
            transport_metrics: CodexTransportMetrics {
                upstream_headers_ms: Some(upstream_headers_ms),
                http_version: Some(http_version),
                ..CodexTransportMetrics::default()
            },
            connection_local_continuation: false,
        })
    }

    pub async fn create_response(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendResponse> {
        self.create_response_started_at(request, context, Instant::now())
            .await
    }

    pub async fn create_response_started_at(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        started_at: Instant,
    ) -> CodexClientResult<CodexBackendResponse> {
        self.create_response_with_pool_account_started_at(request, context, None, started_at)
            .await
    }

    pub async fn create_response_with_pool_account_started_at(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        pool_account_id: Option<&str>,
        started_at: Instant,
    ) -> CodexClientResult<CodexBackendResponse> {
        let prepared = self
            .prepare_response_transport_with_pool_account(request, context, pool_account_id)
            .await?;
        self.create_response_with_prepared(request, context, prepared, started_at)
            .await
    }

    /// 发送 Responses SSE 请求并返回 live SSE 流。
    pub async fn create_response_stream(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendStreamingResponse> {
        self.create_response_stream_with_pool_account(request, context, None)
            .await
    }

    pub async fn create_response_stream_with_pool_account(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        pool_account_id: Option<&str>,
    ) -> CodexClientResult<CodexBackendStreamingResponse> {
        let prepared = self
            .prepare_response_transport_with_pool_account(request, context, pool_account_id)
            .await?;
        self.create_response_stream_with_prepared(request, context, prepared)
            .await
    }

    /// 在发送 payload 前完成 transport 选择和可取消的 WebSocket opening。
    pub(crate) async fn prepare_response_transport_with_pool_account(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        pool_account_id: Option<&str>,
    ) -> CodexClientResult<PreparedResponseTransport> {
        let requirement = transport_requirement(request);
        if requirement == TransportRequirement::HttpRequired {
            return Ok(PreparedResponseTransport {
                requirement,
                route: PreparedResponseRoute::Http,
                metrics: CodexTransportMetrics {
                    decision: Some(CodexTransportDecision::HttpRequired),
                    ..CodexTransportMetrics::default()
                },
            });
        }

        let websocket_request = websocket_upstream_request(request);
        let headers = self.request_headers_for_http_response(&websocket_request, context)?;
        let websocket_create = CodexWebSocketConnection::responses_create_request(
            &self.base_url,
            &generate_key(),
            websocket_header_pairs(&headers),
            &websocket_request,
        )
        .map_err(CodexClientError::WebSocketEncode)?;
        let artifact = websocket_audit_artifact_from_attempt(
            &websocket_request,
            websocket_create.connection().opening_audit_snapshot(),
            websocket_payload_audit_snapshot(&websocket_request),
        );
        if let Err(error) = write_websocket_audit_artifact_from_env(&artifact).await {
            tracing::warn!(error = %error, "Failed to write Codex WebSocket audit artifact");
        }
        let pool_key = self.websocket_pool_key(request, context, pool_account_id);
        let pool_log_context = pool_key.as_ref().map(WebSocketPoolLogContext::from_key);
        let pool = self.websocket_pool.as_deref().zip(pool_key);
        let fast_path_budget = match requirement {
            TransportRequirement::PersistedContinuation
            | TransportRequirement::ExternalUnknown
            | TransportRequirement::NewChain => Some(self.websocket_fast_path_budget),
            TransportRequirement::ExplicitWebSocketWarmup
            | TransportRequirement::ExactWebSocketContinuation => None,
            TransportRequirement::HttpRequired => None,
        };
        let prepare_started_at = Instant::now();
        let prepared = prepare_response_create_request_with_pool(
            &websocket_create,
            pool,
            &self.websocket_origin_breaker,
            &self.websocket_origin_key,
            fast_path_budget,
            requirement.requires_websocket(),
            self.websocket_initial_event_timeout,
        )
        .await;
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error)
                if requirement.allows_pre_send_http_fallback()
                    && error.allows_pre_send_http_fallback() =>
            {
                let decision = http_fallback_decision(&error);
                let wait_ms = elapsed_duration_millis(prepare_started_at.elapsed());
                tracing::warn!(
                    request_id = %context.request_id,
                    account_id = pool_account_id.or(context.account_id).unwrap_or_default(),
                    transport_requirement = requirement.as_str(),
                    transport_decision = decision.as_str(),
                    transport_decision_wait_ms = wait_ms,
                    error = %error,
                    "WebSocket preparation failed before payload send; using same-account HTTP"
                );
                return Ok(PreparedResponseTransport {
                    requirement,
                    route: PreparedResponseRoute::Http,
                    metrics: CodexTransportMetrics {
                        decision: Some(decision),
                        ws_connect_ms: None,
                        transport_decision_wait_ms: Some(wait_ms),
                        ..CodexTransportMetrics::default()
                    },
                });
            }
            Err(error) => return Err(websocket_exchange_error_to_client_error(error)),
        };
        let decision = websocket_success_decision(requirement, &prepared);
        let metrics = CodexTransportMetrics {
            decision: Some(decision),
            ws_connect_ms: prepared.connect_elapsed().map(elapsed_duration_millis),
            transport_decision_wait_ms: Some(elapsed_duration_millis(
                prepared.decision_wait_elapsed(),
            )),
            upstream_headers_ms: prepared.connect_elapsed().map(elapsed_duration_millis),
            first_event_ms: None,
            http_version: Some("HTTP/1.1".to_string()),
        };
        log_websocket_pool_decision(
            context.request_id,
            pool_account_id.or(context.account_id),
            pool_log_context.as_ref(),
            prepared.pool_decision(),
        );
        tracing::info!(
            request_id = %context.request_id,
            account_id = pool_account_id.or(context.account_id).unwrap_or_default(),
            transport_requirement = requirement.as_str(),
            transport_decision = decision.as_str(),
            ws_connect_ms = ?metrics.ws_connect_ms,
            transport_decision_wait_ms = ?metrics.transport_decision_wait_ms,
            "Responses transport prepared"
        );
        Ok(PreparedResponseTransport {
            requirement,
            route: PreparedResponseRoute::WebSocket(Box::new(PreparedWebSocketRoute {
                request: websocket_create,
                prepared,
            })),
            metrics,
        })
    }

    pub(crate) async fn create_response_with_prepared(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        prepared: PreparedResponseTransport,
        started_at: Instant,
    ) -> CodexClientResult<CodexBackendResponse> {
        let PreparedResponseTransport {
            requirement,
            route,
            metrics,
        } = prepared;
        let result = match route {
            PreparedResponseRoute::Http => self
                .create_response_http_sse(request, context, started_at)
                .await
                .map(|mut response| {
                    merge_preparation_metrics(&mut response.transport_metrics, metrics);
                    response
                }),
            PreparedResponseRoute::WebSocket(route) => {
                let PreparedWebSocketRoute { request, prepared } = *route;
                execute_prepared_response_create_request(&request, prepared, started_at)
                    .await
                    .map_err(websocket_exchange_error_to_client_error)
                    .map(|exchange| CodexBackendResponse {
                        body: exchange.body,
                        transport: CodexBackendTransport::WebSocket,
                        usage: exchange.usage,
                        turn_state: exchange.turn_state,
                        set_cookie_headers: exchange.set_cookie_headers,
                        rate_limit_headers: exchange.rate_limit_headers,
                        first_token_ms: exchange.first_token_ms,
                        websocket_pool_decision: exchange.pool_decision,
                        diagnostics: exchange.diagnostics,
                        response_metadata: exchange.response_metadata,
                        transport_metrics: CodexTransportMetrics {
                            first_event_ms: exchange.first_event_ms,
                            ..metrics
                        },
                        connection_local_continuation: exchange.connection_local_continuation,
                    })
            }
        };
        result.inspect_err(|error| {
            tracing::warn!(
                request_id = %context.request_id,
                transport_requirement = requirement.as_str(),
                failure_phase = "post_send_or_explicit_response",
                error = %error,
                "Responses transport failed after preparation; automatic fallback is disabled"
            );
        })
    }

    pub(crate) async fn create_response_stream_with_prepared(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        prepared: PreparedResponseTransport,
    ) -> CodexClientResult<CodexBackendStreamingResponse> {
        let PreparedResponseTransport {
            requirement,
            route,
            metrics,
        } = prepared;
        let result = match route {
            PreparedResponseRoute::Http => self
                .create_response_stream_http_sse(request, context)
                .await
                .map(|mut response| {
                    merge_preparation_metrics(&mut response.transport_metrics, metrics);
                    response
                }),
            PreparedResponseRoute::WebSocket(route) => {
                let PreparedWebSocketRoute { request, prepared } = *route;
                execute_prepared_response_create_request_stream(&request, prepared)
                    .await
                    .map_err(websocket_exchange_error_to_client_error)
                    .map(|exchange| {
                        tracing::info!(
                            request_id = %context.request_id,
                            websocket_connection_id = %exchange.websocket_connection_id,
                            ws_pool = exchange.pool_decision.map_or("unpooled", WebSocketPoolDecision::kind),
                            "WebSocket response stream established"
                        );
                        CodexBackendStreamingResponse {
                            body: Box::pin(
                                exchange
                                    .body
                                    .map_err(post_send_ambiguous)
                                    .map_err(websocket_exchange_error_to_client_error),
                            ),
                            transport: CodexBackendTransport::WebSocket,
                            turn_state: exchange.turn_state,
                            set_cookie_headers: exchange.set_cookie_headers,
                            rate_limit_headers: exchange.rate_limit_headers,
                            rate_limit_header_updates: Some(exchange.rate_limit_header_updates),
                            turn_state_update: Some(exchange.turn_state_update),
                            websocket_pool_decision: exchange.pool_decision,
                            diagnostics: exchange.diagnostics,
                            response_metadata: exchange.response_metadata,
                            transport_metrics: metrics,
                            connection_local_continuation: exchange.connection_local_continuation,
                        }
                    })
            }
        };
        result.inspect_err(|error| {
            tracing::warn!(
                request_id = %context.request_id,
                transport_requirement = requirement.as_str(),
                failure_phase = "post_send_or_explicit_response",
                error = %error,
                "Responses stream transport failed after preparation; automatic fallback is disabled"
            );
        })
    }

    fn websocket_pool_key(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        pool_account_id: Option<&str>,
    ) -> Option<CodexWebSocketPoolKey> {
        let account_id = pool_account_id.or(context.account_id)?;
        let conversation_id = request
            .local_conversation_id
            .as_deref()
            .or(request.previous_response_id())?;
        Some(CodexWebSocketPoolKey::new(
            &self.base_url,
            account_id,
            conversation_id,
        ))
    }

    /// 获取后端模型目录条目。
    pub(super) async fn fetch_models_with_context(
        &self,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<Vec<Value>> {
        let fingerprint = self.fingerprint.current();
        let endpoint = endpoint_url(&self.base_url, "codex/models");
        let headers = self.auxiliary_request_headers(&fingerprint, context)?;
        let response = self
            .client
            .get(endpoint)
            .query(&[("client_version", fingerprint.app_version.as_str())])
            .headers(headers)
            .send()
            .await?;
        let status = response.status();
        let diagnostics = response_meta::diagnostics(Some(status.as_u16()), response.headers());
        let set_cookie_headers = response_meta::set_cookie_headers(response.headers());
        let retry_after_seconds = retry_after_seconds(response.headers(), None);
        let body = response.text().await?;
        if !status.is_success() {
            return Err(CodexClientError::Upstream {
                status,
                retry_after_seconds: retry_after_seconds
                    .or_else(|| retry_after_seconds_from_body(&body)),
                body,
                diagnostics,
                set_cookie_headers,
                transport: CodexBackendTransport::HttpSse,
            });
        }
        let parsed =
            serde_json::from_str::<Value>(&body).map_err(|error| CodexClientError::Upstream {
                status: StatusCode::BAD_GATEWAY,
                retry_after_seconds: None,
                body: format!(
                    "model catalog response is not valid JSON: {error}; body: {}",
                    truncate_for_error(&body)
                ),
                diagnostics: CodexUpstreamDiagnostics::with_status(
                    StatusCode::BAD_GATEWAY.as_u16(),
                ),
                set_cookie_headers,
                transport: CodexBackendTransport::HttpSse,
            })?;
        let models = extract_model_entries(&parsed);
        if !models.is_empty() {
            return Ok(models);
        }

        Err(CodexClientError::Upstream {
            status: StatusCode::BAD_GATEWAY,
            retry_after_seconds: None,
            body: "model catalog response contains no models".to_string(),
            diagnostics: CodexUpstreamDiagnostics::with_status(StatusCode::BAD_GATEWAY.as_u16()),
            set_cookie_headers: Vec::new(),
            transport: CodexBackendTransport::HttpSse,
        })
    }

    fn request_headers_for_http_response(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<HeaderMap> {
        let mut headers = self.request_headers(context)?;
        if let Some(subagent) = openai_subagent_from_metadata(request.client_metadata()) {
            headers.insert(
                HeaderName::from_static("x-openai-subagent"),
                HeaderValue::from_str(&subagent)?,
            );
        }
        Ok(headers)
    }

    fn request_headers(&self, context: CodexRequestContext<'_>) -> CodexClientResult<HeaderMap> {
        let fingerprint = self.fingerprint.current();
        let request_id = context.client_request_id.unwrap_or(context.request_id);
        let ordered_headers = build_ordered_codex_base_headers(
            &fingerprint,
            context.access_token,
            context.account_id,
        );

        let mut headers = HeaderMap::new();
        insert_ordered_headers(&mut headers, &ordered_headers)?;
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        insert_optional_header(&mut headers, "cookie", context.cookie_header)?;
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert(
            HeaderName::from_static("openai-beta"),
            HeaderValue::from_static("responses_websockets=2026-02-06"),
        );
        headers.insert(
            HeaderName::from_static("x-openai-internal-codex-residency"),
            HeaderValue::from_static("us"),
        );
        headers.insert(
            HeaderName::from_static("x-client-request-id"),
            HeaderValue::from_str(request_id)?,
        );
        insert_optional_header(
            &mut headers,
            "x-codex-installation-id",
            context.installation_id,
        )?;
        insert_optional_header(&mut headers, "session-id", context.session_id)?;
        insert_optional_header(&mut headers, "thread-id", context.thread_id)?;
        insert_optional_header(&mut headers, "x-codex-turn-id", context.turn_id)?;
        insert_optional_header(&mut headers, "x-codex-window-id", context.codex_window_id)?;
        insert_optional_header(&mut headers, "x-codex-turn-state", context.turn_state)?;
        insert_optional_header(&mut headers, "x-codex-turn-metadata", context.turn_metadata)?;
        insert_optional_header(&mut headers, "x-codex-beta-features", context.beta_features)?;
        insert_optional_header(
            &mut headers,
            "x-responsesapi-include-timing-metrics",
            context.include_timing_metrics,
        )?;
        insert_optional_header(&mut headers, "version", context.version)?;
        insert_optional_header(
            &mut headers,
            "x-codex-parent-thread-id",
            context.parent_thread_id,
        )?;

        Ok(headers)
    }

    fn auxiliary_request_headers(
        &self,
        fingerprint: &Fingerprint,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<HeaderMap> {
        let ordered_headers =
            build_ordered_codex_base_headers(fingerprint, context.access_token, context.account_id);

        let mut headers = HeaderMap::new();
        insert_ordered_headers(&mut headers, &ordered_headers)?;
        if let Some(cookie_header) = context.cookie_header {
            headers.insert(COOKIE, HeaderValue::from_str(cookie_header)?);
        }
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("gzip, deflate"));
        insert_optional_header(
            &mut headers,
            "x-codex-installation-id",
            context.installation_id,
        )?;
        Ok(headers)
    }

    pub(in crate::upstream::openai::transport) fn usage_request_headers(
        &self,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<HeaderMap> {
        let fingerprint = self.fingerprint.current();
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&fingerprint.user_agent())?,
        );
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", context.access_token))?,
        );
        headers.insert(
            HeaderName::from_static("originator"),
            HeaderValue::from_str(&fingerprint.originator)?,
        );
        insert_optional_header(&mut headers, "chatgpt-account-id", context.account_id)?;
        insert_optional_header(&mut headers, "cookie", context.cookie_header)?;
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        Ok(headers)
    }
}

fn http_sse_stream(response: ReqwestResponse) -> CodexBackendSseStream {
    let stream: CodexBackendSseStream =
        Box::pin(response.bytes_stream().map_err(CodexClientError::Http));
    Box::pin(futures::stream::unfold(Some(stream), |stream| async move {
        let mut stream = stream?;
        match tokio::time::timeout(UPSTREAM_STREAM_IDLE_TIMEOUT, stream.next()).await {
            Ok(Some(chunk)) => Some((chunk, Some(stream))),
            Ok(None) => None,
            Err(_) => Some((
                Err(CodexClientError::StreamIdleTimeout {
                    timeout: UPSTREAM_STREAM_IDLE_TIMEOUT,
                }),
                None,
            )),
        }
    }))
}
