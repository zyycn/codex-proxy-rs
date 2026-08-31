use std::{sync::Arc, time::Instant};

use futures::{StreamExt, TryStreamExt};
use gateway_protocol::openai::{
    X_OPENAI_INTERNAL_CODEX_RESPONSES_LITE_HEADER, X_OPENAI_MEMGEN_REQUEST_HEADER,
    events::{self, retry_after_seconds_from_body},
    sse::{SseEventDecoder, SseFrame},
};
use reqwest::{
    Client, Response as ReqwestResponse,
    header::{
        ACCEPT, AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE, COOKIE, HeaderMap, HeaderName,
        HeaderValue, USER_AGENT,
    },
};
use tokio_tungstenite::tungstenite::handshake::client::generate_key;

use crate::transport::{
    catalog::{
        CodexModelCatalogError, CodexModelCatalogSnapshot, MAX_CODEX_MODEL_CATALOG_BYTES,
        catalog_etag, parse_codex_model_catalog,
    },
    diagnostics::CodexUpstreamSendPhase,
    endpoints::{CODEX_RESPONSES_PATH, endpoint_url},
    headers::{
        build_codex_base_headers, insert_optional_header, insert_optional_protocol_header,
        websocket_header_pairs,
    },
    profile::{CodexWireProfile, CodexWireProfileState},
    protocol::{
        responses::{CodexResponsesRequest, TransportRequirement, transport_requirement},
        websocket::{
            websocket_audit_artifact_from_attempt, websocket_connection_limit_message,
            websocket_payload_audit_snapshot,
        },
    },
    response_meta,
    websocket::{
        CodexWebSocketConnection, CodexWebSocketExchangeError, CodexWebSocketPool,
        CodexWebSocketPoolKey, CodexWebSocketStreamingExchange, DEFAULT_INITIAL_EVENT_TIMEOUT,
        WEBSOCKET_FAST_PATH_BUDGET, WebSocketOriginBreaker, WebSocketPoolDecision,
        execute_prepared_response_create_request_stream, post_send_ambiguous,
        prepare_response_create_request_with_pool, websocket_audit_dir,
        write_websocket_audit_artifact_from_env,
    },
};

use super::client::*;

impl CodexBackendClient {
    /// 构造客户端。
    pub fn new(
        client: Client,
        base_url: impl Into<String>,
        profile: CodexWireProfileState,
    ) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        Self {
            client,
            websocket_origin_key: websocket_origin_key(&base_url),
            base_url,
            profile,
            websocket_pool: None,
            websocket_origin_breaker: WebSocketOriginBreaker::default(),
        }
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

    /// 发送 Responses SSE 请求并返回 live SSE 流（HTTP SSE fallback）。
    pub(crate) async fn create_response_stream_http_sse(
        &self,
        upstream_request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexBackendStreamingResponse> {
        let headers = self.request_headers_for_http_response(upstream_request, context)?;
        let headers_started_at = Instant::now();
        // 与官方 Codex app-server 一致：/v1/responses 请求体默认 zstd 压缩。
        // Codex 上游只交付 SSE；即使下游请求 `stream: false`，也要上游流式执行，
        // 再由 API 层收集 canonical events 并返回完整 JSON。不能把下游的传输偏好
        // 直接透传给 Codex，否则上游会以 400 拒绝非流式请求。
        let mut upstream_body = upstream_request.body().clone();
        upstream_body.insert("stream".to_owned(), serde_json::Value::Bool(true));
        let body =
            serde_json::to_vec(&upstream_body).map_err(CodexClientError::RequestBodyEncode)?;
        let body = zstd::stream::encode_all(std::io::Cursor::new(body), 3)
            .map_err(CodexClientError::RequestCompression)?;
        let response = self
            .client
            .post(endpoint_url(&self.base_url, CODEX_RESPONSES_PATH))
            .headers(headers)
            .header(CONTENT_ENCODING, HeaderValue::from_static("zstd"))
            .body(body)
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
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .map(|value| value.as_bytes().to_vec());
            let client_headers = response_meta::client_headers(response.headers());
            let raw_body = read_error_response_body(response).await?;
            let body = String::from_utf8_lossy(&raw_body).into_owned();
            let retry_after_seconds =
                retry_after_seconds.or_else(|| retry_after_seconds_from_body(&body));
            return Err(CodexClientError::Upstream {
                status,
                body,
                client_response: Some(Box::new(CodexClientVisibleUpstreamResponse::new(
                    status,
                    content_type,
                    client_headers,
                    raw_body,
                ))),
                retry_after_seconds,
                diagnostics: Box::new(diagnostics),
                set_cookie_headers,
                rate_limit_headers,
                transport: CodexBackendTransport::HttpSse,
                transport_metrics: Box::new(CodexTransportMetrics {
                    upstream_headers_ms: Some(upstream_headers_ms),
                    http_version: Some(http_version),
                    ..CodexTransportMetrics::default()
                }),
                send_phase: CodexUpstreamSendPhase::AfterPayload,
            });
        }

        let rate_limit_updates = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        Ok(CodexBackendStreamingResponse {
            body: http_sse_stream(response, Arc::clone(&rate_limit_updates)),
            transport: CodexBackendTransport::HttpSse,
            websocket_connection_id: None,
            turn_state,
            set_cookie_headers,
            rate_limit_headers,
            rate_limit_updates: Some(rate_limit_updates),
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
    #[doc(hidden)]
    pub(crate) async fn prepare_response_transport_with_pool_account(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        pool_account_id: Option<&str>,
    ) -> CodexClientResult<PreparedResponseTransport> {
        self.prepare_response_transport(request, context, pool_account_id, false)
            .await
    }

    /// 与 [`Self::prepare_response_transport_with_pool_account`] 相同；
    /// `force_fresh` 时绕过连接池，为连接寿命限制重试强制新建连接。
    async fn prepare_response_transport(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        pool_account_id: Option<&str>,
        force_fresh: bool,
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
        let headers = self.request_headers_for_websocket_response(&websocket_request, context)?;
        let websocket_create = CodexWebSocketConnection::responses_create_request(
            &self.base_url,
            &generate_key(),
            websocket_header_pairs(&headers),
            &websocket_request,
        )
        .map_err(CodexClientError::WebSocketEncode)?;
        // 审计未启用时跳过 artifact 构造：payload 快照会深拷贝整个请求 body，
        // 且位于首字节前的关键路径上。
        if websocket_audit_dir().is_some() {
            let artifact = websocket_audit_artifact_from_attempt(
                &websocket_request,
                websocket_create.connection().opening_audit_snapshot(),
                websocket_payload_audit_snapshot(&websocket_request),
            );
            if let Err(error) = write_websocket_audit_artifact_from_env(&artifact).await {
                tracing::warn!(error = %error, "Failed to write Codex WebSocket audit artifact");
            }
        }
        let connection_profile = websocket_connection_profile(&headers);
        let pool_key =
            self.websocket_pool_key(request, context, pool_account_id, &connection_profile);
        let pool_log_context = pool_key.as_ref().map(WebSocketPoolLogContext::from_key);
        let pool = if force_fresh {
            None
        } else {
            self.websocket_pool.as_deref().zip(pool_key)
        };
        let fast_path_budget = match requirement {
            TransportRequirement::PersistedContinuation | TransportRequirement::NewChain => {
                Some(WEBSOCKET_FAST_PATH_BUDGET)
            }
            TransportRequirement::ExplicitWebSocketWarmup
            | TransportRequirement::ExactWebSocketContinuation
            | TransportRequirement::ExternalUnknown => None,
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
            Some(DEFAULT_INITIAL_EVENT_TIMEOUT),
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
            context,
            pool_account_id,
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

    #[doc(hidden)]
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
        match route {
            PreparedResponseRoute::Http => self
                .create_response_stream_http_sse(request, context)
                .await
                .map(|mut response| {
                    merge_preparation_metrics(&mut response.transport_metrics, metrics);
                    response
                }),
            PreparedResponseRoute::WebSocket(route) => {
                let PreparedWebSocketRoute {
                    request: websocket_request,
                    prepared,
                } = *route;
                let delivery_wait_started_at = Instant::now();
                let mut exchange = match execute_prepared_response_create_request_stream(
                    &websocket_request,
                    prepared,
                )
                .await
                {
                    Ok(exchange) => exchange,
                    Err(error)
                        if requirement.allows_pre_delivery_http_fallback()
                            && error.allows_pre_delivery_http_fallback() =>
                    {
                        return self
                            .fallback_to_http_before_websocket_delivery(
                                request,
                                context,
                                requirement,
                                metrics,
                                delivery_wait_started_at,
                                error,
                            )
                            .await;
                    }
                    Err(error) => return Err(websocket_exchange_error_to_client_error(error)),
                };
                // 仅普通新链请求允许交付前降级/重试；延续类请求保持旧语义：
                // 不进入交付边界，错误由 exchange 流原样上抛给下游。
                if requirement.allows_pre_delivery_http_fallback() {
                    let mut fresh_retry_used = false;
                    loop {
                        match await_websocket_delivery_boundary(&mut exchange).await {
                            Ok(DeliveryBoundary::Ready) => break,
                            Ok(DeliveryBoundary::ConnectionLimitReached {
                                message,
                                raw_frame,
                            }) => {
                                let upstream_error_raw = String::from_utf8_lossy(&raw_frame);
                                tracing::warn!(
                                    request_id = %context.request_id,
                                    account_id = context.account_id.unwrap_or_default(),
                                    websocket_connection_id = %exchange.websocket_connection_id,
                                    upstream_transport = "websocket",
                                    upstream_error_kind = "websocket_error_frame",
                                    upstream_error_raw = %upstream_error_raw,
                                    "OpenAI upstream returned a recoverable WebSocket error frame"
                                );
                                if fresh_retry_used {
                                    // 新连接仍被限流：与官方重试预算耗尽一致，降级同账号 HTTP/SSE。
                                    return self
                                        .http_fallback_before_delivery(
                                            request,
                                            context,
                                            requirement,
                                            metrics,
                                            delivery_wait_started_at,
                                            &message,
                                        )
                                        .await
                                        ;
                                }
                                fresh_retry_used = true;
                                tracing::warn!(
                                    request_id = %context.request_id,
                                    %message,
                                    "WebSocket connection limit reached; retrying on a fresh connection"
                                );
                                drop(exchange);
                                match self
                                    .prepare_response_transport(request, context, None, true)
                                    .await
                                {
                                    Ok(PreparedResponseTransport {
                                        route: PreparedResponseRoute::WebSocket(route),
                                        ..
                                    }) => {
                                        match execute_prepared_response_create_request_stream(
                                            &route.request,
                                            route.prepared,
                                        )
                                        .await
                                        {
                                            Ok(next_exchange) => {
                                                exchange = next_exchange;
                                                continue;
                                            }
                                            Err(error)
                                                if error.allows_pre_delivery_http_fallback() =>
                                            {
                                                return self
                                                    .fallback_to_http_before_websocket_delivery(
                                                        request,
                                                        context,
                                                        requirement,
                                                        metrics,
                                                        delivery_wait_started_at,
                                                        error,
                                                    )
                                                    .await;
                                            }
                                            Err(error) => {
                                                return Err(
                                                    websocket_exchange_error_to_client_error(
                                                        post_send_ambiguous(error),
                                                    ),
                                                );
                                            }
                                        }
                                    }
                                    Ok(PreparedResponseTransport {
                                        route: PreparedResponseRoute::Http,
                                        ..
                                    }) => {
                                        return self
                                            .create_response_stream_http_sse(request, context)
                                            .await
                                            .map(|mut response| {
                                                merge_preparation_metrics(
                                                    &mut response.transport_metrics,
                                                    metrics,
                                                );
                                                response
                                            });
                                    }
                                    Err(error) => {
                                        return self
                                            .http_fallback_before_delivery(
                                                request,
                                                context,
                                                requirement,
                                                metrics,
                                                delivery_wait_started_at,
                                                &error.to_string(),
                                            )
                                            .await;
                                    }
                                }
                            }
                            Err(error) => {
                                if error.allows_pre_delivery_http_fallback() {
                                    return self
                                        .fallback_to_http_before_websocket_delivery(
                                            request,
                                            context,
                                            requirement,
                                            metrics,
                                            delivery_wait_started_at,
                                            error,
                                        )
                                        .await;
                                }
                                return Err(websocket_exchange_error_to_client_error(
                                    post_send_ambiguous(error),
                                ));
                            }
                        }
                    }
                }
                tracing::info!(
                    request_id = %context.request_id,
                    websocket_connection_id = %exchange.websocket_connection_id,
                    ws_pool = exchange.pool_decision.map_or("unpooled", WebSocketPoolDecision::kind),
                    "WebSocket response stream established"
                );
                Ok(CodexBackendStreamingResponse {
                    body: Box::pin(
                        exchange
                            .body
                            .map_err(post_send_ambiguous)
                            .map_err(websocket_exchange_error_to_client_error),
                    ),
                    transport: CodexBackendTransport::WebSocket,
                    websocket_connection_id: Some(exchange.websocket_connection_id),
                    turn_state: exchange.turn_state,
                    set_cookie_headers: exchange.set_cookie_headers,
                    rate_limit_headers: exchange.rate_limit_headers,
                    rate_limit_updates: Some(exchange.rate_limit_updates),
                    turn_state_update: Some(exchange.turn_state_update),
                    websocket_pool_decision: exchange.pool_decision,
                    diagnostics: exchange.diagnostics,
                    response_metadata: exchange.response_metadata,
                    transport_metrics: metrics,
                    connection_local_continuation: exchange.connection_local_continuation,
                })
            }
        }
        .inspect_err(|error| {
            tracing::warn!(
                request_id = %context.request_id,
                transport_requirement = requirement.as_str(),
                failure_phase = "post_send_or_explicit_response",
                error = %error,
                "Responses stream transport failed after preparation"
            );
        })
    }

    async fn fallback_to_http_before_websocket_delivery(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        requirement: TransportRequirement,
        metrics: CodexTransportMetrics,
        delivery_wait_started_at: Instant,
        error: CodexWebSocketExchangeError,
    ) -> CodexClientResult<CodexBackendStreamingResponse> {
        log_raw_websocket_close(context, &error);
        self.http_fallback_before_delivery(
            request,
            context,
            requirement,
            metrics,
            delivery_wait_started_at,
            &error.to_string(),
        )
        .await
    }

    /// 交付前降级同账号 HTTP/SSE 的公共路径：统一标记 decision 与等待耗时。
    async fn http_fallback_before_delivery(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        requirement: TransportRequirement,
        mut metrics: CodexTransportMetrics,
        delivery_wait_started_at: Instant,
        detail: &str,
    ) -> CodexClientResult<CodexBackendStreamingResponse> {
        let decision_wait_ms = metrics
            .transport_decision_wait_ms
            .unwrap_or_default()
            .saturating_add(elapsed_duration_millis(delivery_wait_started_at.elapsed()));
        metrics.decision = Some(CodexTransportDecision::Http2PreDeliveryFailure);
        metrics.transport_decision_wait_ms = Some(decision_wait_ms);
        tracing::warn!(
            request_id = %context.request_id,
            account_id = context.account_id.unwrap_or_default(),
            transport_requirement = requirement.as_str(),
            transport_decision = CodexTransportDecision::Http2PreDeliveryFailure.as_str(),
            transport_decision_wait_ms = decision_wait_ms,
            error = %detail,
            "WebSocket failed before first deliverable event; using same-account HTTP"
        );
        self.create_response_stream_http_sse(request, context)
            .await
            .map(|mut response| {
                merge_preparation_metrics(&mut response.transport_metrics, metrics);
                response
            })
    }

    fn websocket_pool_key(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
        pool_account_id: Option<&str>,
        connection_profile: &str,
    ) -> Option<CodexWebSocketPoolKey> {
        let account_id = pool_account_id.or(context.account_id)?;
        let conversation_id = request
            .local_conversation_id
            .as_deref()
            .or(request.previous_response_id())?;
        Some(
            CodexWebSocketPoolKey::new(&self.base_url, account_id, conversation_id)
                .with_connection_profile(connection_profile),
        )
    }

    /// 获取后端模型目录条目。
    pub async fn fetch_models_with_context(
        &self,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<CodexModelCatalogSnapshot> {
        let endpoint = endpoint_url(&self.base_url, "codex/models");
        let profile = self.profile.snapshot();
        let headers = self.auxiliary_request_headers(&profile, context)?;
        let response = self
            .client
            .get(endpoint)
            .query(&[("client_version", profile.codex_version.as_str())])
            .headers(headers)
            .send()
            .await?;
        let status = response.status();
        let diagnostics = response_meta::diagnostics(Some(status.as_u16()), response.headers());
        let set_cookie_headers = response_meta::set_cookie_headers(response.headers());
        let retry_after_seconds = retry_after_seconds(response.headers(), None);
        let etag = status
            .is_success()
            .then(|| catalog_etag(response.headers()))
            .transpose()?
            .flatten();
        let body = read_model_catalog_body(response).await?;
        if !status.is_success() {
            let body = String::from_utf8_lossy(&body).into_owned();
            return Err(CodexClientError::Upstream {
                status,
                retry_after_seconds: retry_after_seconds
                    .or_else(|| retry_after_seconds_from_body(&body)),
                body,
                client_response: None,
                diagnostics: Box::new(diagnostics),
                set_cookie_headers,
                rate_limit_headers: Vec::new(),
                transport: CodexBackendTransport::HttpSse,
                transport_metrics: Box::default(),
                send_phase: CodexUpstreamSendPhase::AfterPayload,
            });
        }
        Ok(parse_codex_model_catalog(&body, etag.as_deref())?)
    }

    fn request_headers_for_http_response(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<HeaderMap> {
        let profile = self.profile.snapshot();
        let mut headers = self.request_headers(&profile, context)?;
        if let Some(subagent) = openai_subagent_from_metadata(request.client_metadata()) {
            insert_optional_protocol_header(&mut headers, "x-openai-subagent", Some(&subagent));
        }
        insert_optional_protocol_header(
            &mut headers,
            X_OPENAI_INTERNAL_CODEX_RESPONSES_LITE_HEADER,
            request.responses_lite.as_deref(),
        );
        insert_optional_protocol_header(
            &mut headers,
            X_OPENAI_MEMGEN_REQUEST_HEADER,
            request.memgen_request.as_deref(),
        );
        // 客户端确实携带的普通协议头优先；没有携带时才使用上面的 Codex profile 默认值。
        for name in request.passthrough_headers.keys() {
            // 上游指纹必须由运行时画像统一生成：originator/User-Agent 即使绕过
            // API 透传黑名单也不能覆盖画像，避免下游客户端暴露不一致指纹。
            if matches!(
                name.as_str(),
                "openai-beta"
                    | "originator"
                    | "user-agent"
                    | "x-oai-attestation"
                    | "x-oai-is"
                    | "x-oai-is-update"
                    | "x-openai-internal-codex-residency"
            ) {
                continue;
            }
            headers.remove(name);
            for value in request.passthrough_headers.get_all(name) {
                headers.append(name.clone(), value.clone());
            }
        }
        Ok(headers)
    }

    fn request_headers_for_websocket_response(
        &self,
        request: &CodexResponsesRequest,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<HeaderMap> {
        let mut headers = self.request_headers_for_http_response(request, context)?;
        headers.remove(X_OPENAI_INTERNAL_CODEX_RESPONSES_LITE_HEADER);
        headers.insert(
            HeaderName::from_static("openai-beta"),
            HeaderValue::from_static("responses_websockets=2026-02-06"),
        );
        Ok(headers)
    }

    fn request_headers(
        &self,
        profile: &CodexWireProfile,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<HeaderMap> {
        let mut headers =
            build_codex_base_headers(profile, context.authorization, context.account_id)?;
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        insert_optional_header(&mut headers, "cookie", context.cookie_header)?;
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert(
            HeaderName::from_static("x-client-request-id"),
            HeaderValue::from_str(context.request_id)?,
        );
        insert_optional_protocol_header(
            &mut headers,
            "x-client-request-id",
            context.client_request_id,
        );
        insert_optional_header(
            &mut headers,
            "x-codex-installation-id",
            context.installation_id,
        )?;
        insert_optional_protocol_header(&mut headers, "session-id", context.session_id);
        insert_optional_protocol_header(&mut headers, "thread-id", context.thread_id);
        insert_optional_protocol_header(&mut headers, "x-codex-turn-id", context.turn_id);
        insert_optional_protocol_header(&mut headers, "x-codex-window-id", context.codex_window_id);
        insert_optional_protocol_header(&mut headers, "x-codex-turn-state", context.turn_state);
        insert_optional_protocol_header(
            &mut headers,
            "x-codex-turn-metadata",
            context.turn_metadata,
        );
        insert_optional_protocol_header(
            &mut headers,
            "x-codex-beta-features",
            context.beta_features,
        );
        insert_optional_protocol_header(
            &mut headers,
            "x-responsesapi-include-timing-metrics",
            context.include_timing_metrics,
        );
        insert_optional_protocol_header(&mut headers, "version", context.version);
        insert_optional_protocol_header(
            &mut headers,
            "x-codex-parent-thread-id",
            context.parent_thread_id,
        );

        Ok(headers)
    }

    fn auxiliary_request_headers(
        &self,
        profile: &CodexWireProfile,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<HeaderMap> {
        let mut headers =
            build_codex_base_headers(profile, context.authorization, context.account_id)?;
        if let Some(cookie_header) = context.cookie_header {
            headers.insert(COOKIE, HeaderValue::from_str(cookie_header)?);
        }
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        insert_optional_header(
            &mut headers,
            "x-codex-installation-id",
            context.installation_id,
        )?;
        Ok(headers)
    }

    pub(in crate::transport) fn usage_request_headers(
        &self,
        context: CodexRequestContext<'_>,
    ) -> CodexClientResult<HeaderMap> {
        let profile = self.profile.snapshot();
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_str(&profile.user_agent())?);
        headers.insert(AUTHORIZATION, HeaderValue::from_str(context.authorization)?);
        insert_optional_header(&mut headers, "chatgpt-account-id", context.account_id)?;
        insert_optional_header(&mut headers, "cookie", context.cookie_header)?;
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        Ok(headers)
    }
}

/// 首个可投递帧前的交付边界结果。
enum DeliveryBoundary {
    /// 已越过边界，可开始向下游投递。
    Ready,
    /// 首个可投递帧是上游连接寿命限制错误；该帧已放回流，
    /// 调用方可选择丢弃并换新连接重试，或原样投递。
    ConnectionLimitReached {
        /// 上游错误说明。
        message: String,
        /// 从 WebSocket 文本事件构造的完整 SSE 错误帧。
        raw_frame: bytes::Bytes,
    },
}

async fn await_websocket_delivery_boundary(
    exchange: &mut CodexWebSocketStreamingExchange,
) -> Result<DeliveryBoundary, CodexWebSocketExchangeError> {
    let mut prelude = Vec::new();
    loop {
        match exchange.body.next().await {
            Some(Ok(frame)) if is_websocket_lifecycle_prelude(&frame) => prelude.push(frame),
            Some(Ok(frame)) => {
                let limit_message = websocket_connection_limit_message(&frame);
                let raw_frame = limit_message.as_ref().map(|_| frame.clone());
                prelude.push(frame);
                let remaining =
                    std::mem::replace(&mut exchange.body, Box::pin(futures::stream::empty()));
                exchange.body =
                    Box::pin(futures::stream::iter(prelude.into_iter().map(Ok)).chain(remaining));
                return Ok(match limit_message {
                    Some(message) => DeliveryBoundary::ConnectionLimitReached {
                        message,
                        raw_frame: raw_frame.unwrap_or_default(),
                    },
                    None => DeliveryBoundary::Ready,
                });
            }
            Some(Err(error)) => return Err(error),
            None => {
                return Err(CodexWebSocketExchangeError::closed_before_terminal_on(
                    exchange.websocket_connection_id,
                    None,
                    None,
                ));
            }
        }
    }
}

fn log_raw_websocket_close(context: CodexRequestContext<'_>, error: &CodexWebSocketExchangeError) {
    let Some(close) = error.close_before_terminal() else {
        return;
    };
    let websocket_connection_id = close
        .connection_id()
        .map(|connection_id| connection_id.to_string())
        .unwrap_or_default();
    let upstream_error_raw = close.reason().unwrap_or_default();
    tracing::warn!(
        request_id = %context.request_id,
        account_id = context.account_id.unwrap_or_default(),
        websocket_connection_id,
        upstream_transport = "websocket",
        upstream_error_kind = "websocket_close",
        upstream_close_code = ?close.code(),
        upstream_error_raw,
        upstream_error_raw_present = close.reason().is_some(),
        "OpenAI upstream WebSocket closed before delivery"
    );
}

fn is_websocket_lifecycle_prelude(frame: &[u8]) -> bool {
    frame.starts_with(b"event: response.created\n")
        || frame.starts_with(b"event: response.in_progress\n")
}

async fn read_model_catalog_body(response: ReqwestResponse) -> CodexClientResult<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_CODEX_MODEL_CATALOG_BYTES as u64)
    {
        return Err(CodexModelCatalogError::ResponseTooLarge.into());
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let Some(next_len) = body.len().checked_add(chunk.len()) else {
            return Err(CodexModelCatalogError::ResponseTooLarge.into());
        };
        if next_len > MAX_CODEX_MODEL_CATALOG_BYTES {
            return Err(CodexModelCatalogError::ResponseTooLarge.into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn websocket_connection_profile(headers: &HeaderMap) -> String {
    ["originator", "user-agent", X_OPENAI_MEMGEN_REQUEST_HEADER]
        .map(|name| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
        })
        .join("\0")
}

fn http_sse_stream(
    response: ReqwestResponse,
    rate_limit_updates: CodexRateLimitUpdates,
) -> CodexBackendSseStream {
    let stream: CodexBackendSseStream =
        Box::pin(response.bytes_stream().map_err(CodexClientError::Http));
    let stream: CodexBackendSseStream =
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
        }));
    observe_http_sse_rate_limits(stream, rate_limit_updates)
}

fn observe_http_sse_rate_limits(
    stream: CodexBackendSseStream,
    updates: CodexRateLimitUpdates,
) -> CodexBackendSseStream {
    Box::pin(futures::stream::unfold(
        (stream, SseEventDecoder::default(), updates),
        |(mut stream, mut decoder, updates)| async move {
            match stream.next().await {
                Some(chunk) => {
                    if let Ok(bytes) = &chunk {
                        append_http_sse_rate_limit_updates(decoder.push_frames(bytes), &updates)
                            .await;
                    }
                    Some((chunk, (stream, decoder, updates)))
                }
                None => {
                    append_http_sse_rate_limit_updates(decoder.finish_frames(), &updates).await;
                    None
                }
            }
        },
    ))
}

async fn append_http_sse_rate_limit_updates(
    frames: Vec<SseFrame>,
    updates: &CodexRateLimitUpdates,
) {
    let mut observations = Vec::new();
    for frame in frames {
        for event in frame.events() {
            if event
                .event
                .as_deref()
                .is_some_and(|event| event != "codex.rate_limits")
            {
                continue;
            }
            let Some(rate_limits) = events::parse_rate_limits_event_raw(&event.data) else {
                continue;
            };
            observations.push(rate_limits);
        }
    }
    if !observations.is_empty() {
        updates.lock().await.extend(observations);
    }
}
