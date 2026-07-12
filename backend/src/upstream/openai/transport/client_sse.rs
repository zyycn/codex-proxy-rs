use super::*;

impl CodexBackendClient {
    /// 构造客户端。
    pub fn new(
        client: Client,
        base_url: impl Into<String>,
        fingerprint: RuntimeFingerprint,
    ) -> Self {
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            fingerprint,
            websocket_pool: None,
            websocket_initial_event_timeout: Some(DEFAULT_INITIAL_EVENT_TIMEOUT),
        }
    }

    pub fn with_websocket_initial_event_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.websocket_initial_event_timeout = timeout.filter(|timeout| !timeout.is_zero());
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
        let response = self
            .client
            .post(endpoint_url(&self.base_url, CODEX_RESPONSES_PATH))
            .headers(headers)
            .json(&upstream_request)
            .send()
            .await?;
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
            });
        }

        let mut body_bytes = Vec::new();
        let mut first_token_ms = None;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.try_next().await? {
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
        })
    }

    /// 发送 Responses SSE 请求并返回 live SSE 流（HTTP SSE fallback）。
    async fn create_response_stream_http_sse(
        &self,
        upstream_request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendStreamingResponse> {
        let headers = self.request_headers_for_http_response(upstream_request, context)?;
        let response = self
            .client
            .post(endpoint_url(&self.base_url, CODEX_RESPONSES_PATH))
            .headers(headers)
            .json(&upstream_request)
            .send()
            .await?;
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
            });
        }

        Ok(CodexBackendStreamingResponse {
            body: Box::pin(response.bytes_stream().map_err(CodexClientError::Http)),
            transport: CodexBackendTransport::HttpSse,
            turn_state,
            set_cookie_headers,
            rate_limit_headers,
            rate_limit_header_updates: None,
            turn_state_update: None,
            websocket_pool_decision: None,
            diagnostics,
            response_metadata,
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
        let upstream_request = response_upstream_request(request, context);
        match transport_for_request(&upstream_request) {
            CodexTransport::HttpSse => {
                self.create_response_http_sse(&upstream_request, context, started_at)
                    .await
            }
            CodexTransport::WebSocketPreferred | CodexTransport::WebSocketRequired => {
                match self
                    .create_response_websocket(
                        &upstream_request,
                        context,
                        pool_account_id,
                        started_at,
                    )
                    .await
                {
                    Ok(response) => Ok(response),
                    Err(error)
                        if http_sse_fallback_allowed(&upstream_request)
                            && websocket_error_allows_http_fallback(&error) =>
                    {
                        tracing::warn!(
                            request_id = %context.request_id,
                            account_id = pool_account_id.or(context.account_id).unwrap_or_default(),
                            transport = "websocket",
                            fallback_transport = "http_sse",
                            fallback_reason = "websocket_error",
                            error = %error,
                            "websocket response failed; falling back to HTTP SSE"
                        );
                        self.create_response_http_sse(&upstream_request, context, started_at)
                            .await
                    }
                    Err(error) => Err(error),
                }
            }
        }
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
        let upstream_request = response_upstream_request(request, context);
        match transport_for_request(&upstream_request) {
            CodexTransport::HttpSse => {
                self.create_response_stream_http_sse(&upstream_request, context)
                    .await
            }
            CodexTransport::WebSocketPreferred | CodexTransport::WebSocketRequired => {
                match self
                    .create_response_websocket_stream(&upstream_request, context, pool_account_id)
                    .await
                {
                    Ok(response) => Ok(response),
                    Err(error)
                        if http_sse_fallback_allowed(&upstream_request)
                            && websocket_error_allows_http_fallback(&error) =>
                    {
                        tracing::warn!(
                            request_id = %context.request_id,
                            account_id = pool_account_id.or(context.account_id).unwrap_or_default(),
                            transport = "websocket",
                            fallback_transport = "http_sse",
                            fallback_reason = "websocket_error",
                            error = %error,
                            "websocket response stream failed; falling back to HTTP SSE"
                        );
                        self.create_response_stream_http_sse(&upstream_request, context)
                            .await
                    }
                    Err(error) => Err(error),
                }
            }
        }
    }

    async fn create_response_websocket(
        &self,
        upstream_request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        pool_account_id: Option<&str>,
        started_at: Instant,
    ) -> CodexClientResult<CodexBackendResponse> {
        let websocket_request = websocket_upstream_request(upstream_request);
        let headers = self.request_headers_for_http_response(&websocket_request, context)?;
        let prepared = CodexWebSocketConnection::responses_create_request(
            &self.base_url,
            &generate_key(),
            websocket_header_pairs(&headers),
            &websocket_request,
        )
        .map_err(CodexClientError::WebSocketEncode)?;
        let artifact = websocket_audit_artifact_from_attempt(
            &websocket_request,
            prepared.connection().opening_audit_snapshot(),
            websocket_payload_audit_snapshot(&websocket_request),
        );
        if let Err(error) = write_websocket_audit_artifact_from_env(&artifact).await {
            tracing::warn!(error = %error, "failed to write Codex WebSocket audit artifact");
        }
        let pool_key = self.websocket_pool_key(upstream_request, context, pool_account_id);
        let pool_log_context = pool_key.as_ref().map(WebSocketPoolLogContext::from_key);
        let exchange = match (self.websocket_pool.as_deref(), pool_key) {
            (Some(pool), Some(key)) => {
                execute_response_create_request_with_pool(
                    &prepared,
                    Some((pool, key)),
                    started_at,
                    self.websocket_initial_event_timeout,
                )
                .await
            }
            _ => {
                execute_response_create_request_with_pool(
                    &prepared,
                    None,
                    started_at,
                    self.websocket_initial_event_timeout,
                )
                .await
            }
        }
        .map_err(websocket_exchange_error_to_client_error)?;
        log_websocket_pool_decision(
            context.request_id,
            pool_account_id.or(context.account_id),
            pool_log_context.as_ref(),
            exchange.pool_decision,
        );

        Ok(CodexBackendResponse {
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
        })
    }

    async fn create_response_websocket_stream(
        &self,
        upstream_request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        pool_account_id: Option<&str>,
    ) -> CodexClientResult<CodexBackendStreamingResponse> {
        let websocket_request = websocket_upstream_request(upstream_request);
        let headers = self.request_headers_for_http_response(&websocket_request, context)?;
        let prepared = CodexWebSocketConnection::responses_create_request(
            &self.base_url,
            &generate_key(),
            websocket_header_pairs(&headers),
            &websocket_request,
        )
        .map_err(CodexClientError::WebSocketEncode)?;
        let artifact = websocket_audit_artifact_from_attempt(
            &websocket_request,
            prepared.connection().opening_audit_snapshot(),
            websocket_payload_audit_snapshot(&websocket_request),
        );
        if let Err(error) = write_websocket_audit_artifact_from_env(&artifact).await {
            tracing::warn!(error = %error, "failed to write Codex WebSocket audit artifact");
        }
        let pool_key = self.websocket_pool_key(upstream_request, context, pool_account_id);
        let pool_log_context = pool_key.as_ref().map(WebSocketPoolLogContext::from_key);
        let exchange = match (self.websocket_pool.as_deref(), pool_key) {
            (Some(pool), Some(key)) => {
                execute_response_create_request_stream_with_pool(
                    &prepared,
                    Some((pool, key)),
                    self.websocket_initial_event_timeout,
                )
                .await
            }
            _ => {
                execute_response_create_request_stream_with_pool(
                    &prepared,
                    None,
                    self.websocket_initial_event_timeout,
                )
                .await
            }
        }
        .map_err(websocket_exchange_error_to_client_error)?;
        tracing::info!(
            request_id = %context.request_id,
            account_id = pool_account_id.or(context.account_id).unwrap_or_default(),
            websocket_connection_id = %exchange.websocket_connection_id,
            ws_pool = exchange.pool_decision.map_or("unpooled", WebSocketPoolDecision::kind),
            "websocket response stream established"
        );
        log_websocket_pool_decision(
            context.request_id,
            pool_account_id.or(context.account_id),
            pool_log_context.as_ref(),
            exchange.pool_decision,
        );

        Ok(CodexBackendStreamingResponse {
            body: Box::pin(
                exchange
                    .body
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
