//! 官方 Messages 使用原生计数，其他上游按 CPA 的内容投影做本地 BPE 估算

use std::sync::OnceLock;

use gateway_core::operation::TokenCountRequest;

use super::*;

impl CodexProvider {
    pub(in crate::provider) async fn execute_count_tokens(
        self: Arc<Self>,
        request: &TokenCountRequest,
        candidate: &ProviderCandidate,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        let response_protocol = match request.payload().protocol() {
            ANTHROPIC_PROTOCOL => ANTHROPIC_PROTOCOL,
            "token-count" => "token-count",
            _ => return Err(invalid_request()),
        };
        let model = candidate
            .upstream_model()
            .cloned()
            .ok_or_else(invalid_request)?;
        let started = Instant::now();
        let lease = self
            .selector
            .select(&SelectCodexCredential {
                upstream_model: model.as_str(),
                request_url: &self.responses_url,
                attempt: &context,
                session_affinity_key: None,
            })
            .await
            .map_err(map_selection_error)?;
        let account_selection_wait_ms =
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let provider_kind = ProviderKind::new(PROVIDER_NAME).map_err(|_| invalid_request())?;
        let account_id = lease.account_id().clone();
        let terminal_context = context.clone();
        context
            .execute_middleware(
                Operation::CountTokens(request.clone()),
                provider_kind,
                Some(model.as_str().to_owned()),
                account_id,
                Box::new(move |operation, headers| {
                    Box::pin(async move {
                        let Operation::CountTokens(count) = operation else {
                            return Err(invalid_request());
                        };
                        if count.payload().protocol() != response_protocol {
                            return Err(invalid_request());
                        }
                        let mut body: Map<String, Value> =
                            serde_json::from_slice(count.payload().body())
                                .map_err(|_| invalid_request())?;
                        // 通用 Provider 计数入口的模型来自路径，正文仍使用 Messages 输入合同
                        body.insert("model".to_owned(), Value::String(model.as_str().to_owned()));
                        gateway_protocol::anthropic::validate_count_request(&body)
                            .map_err(|_| invalid_request())?;
                        let native = match lease.authentication() {
                            CodexRuntimeAuthentication::ApiKey(auth)
                                if auth.configuration.protocol == ApiKeyProtocol::Messages =>
                            {
                                Url::parse(&auth.configuration.base_url)
                                    .ok()
                                    .is_some_and(|url| {
                                        url.scheme() == "https"
                                            && url.host_str() == Some("api.anthropic.com")
                                            && url.port_or_known_default() == Some(443)
                                    })
                            }
                            _ => false,
                        };
                        if native {
                            self.messages_stream(
                                body,
                                count.payload().context().clone(),
                                headers,
                                model,
                                terminal_context,
                                lease,
                                account_selection_wait_ms,
                                Some(response_protocol),
                            )
                        } else {
                            self.local_count_stream(
                                body,
                                model,
                                terminal_context,
                                lease,
                                account_selection_wait_ms,
                                response_protocol,
                            )
                        }
                    })
                }),
            )
            .await
    }

    fn local_count_stream(
        &self,
        body: Map<String, Value>,
        model: UpstreamModelId,
        context: AttemptContext,
        lease: CodexCredentialLease,
        account_selection_wait_ms: u64,
        response_protocol: &'static str,
    ) -> Result<ProviderStream, ProviderError> {
        let protocol = lease.authentication().api_protocol();
        let metadata = ProviderCallMetadata::new(
            ProviderKind::new(PROVIDER_NAME).map_err(|_| invalid_request())?,
            model.clone(),
            lease.account_id().clone(),
            UpstreamTransport::new("local_tokenizer").map_err(|_| invalid_request())?,
        )
        .with_credential_revision(lease.account().revision())
        .with_account_snapshot(Some(
            gateway_core::account::AccountHistorySnapshot::from_account(lease.account()),
        ))
        .with_selection_observation(ProviderSelectionObservation::new(
            account_selection_wait_ms,
            lease.capacity_snapshot(),
        ));
        let tokenizer_model = model.as_str().to_ascii_lowercase();
        let events: EventStream = Box::pin(async_stream::try_stream! {
            // BPE 是 CPU 密集任务，按可用核数限制进程内并发；断开后由工作线程持有名额直到退出
            static WORKERS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
            let workers = WORKERS.get_or_init(|| tokio::sync::Semaphore::new(
                std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
            ));
            let permit = tokio::select! {
                biased;
                _ = context.cancellation().cancelled() => Err(provider_error(ProviderErrorKind::Cancelled, UpstreamSendState::NotSent)),
                _ = context.deadline().wait() => Err(provider_error(ProviderErrorKind::Timeout, UpstreamSendState::NotSent)),
                result = workers.acquire() => result.map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)),
            }?;
            let cancellation = context.cancellation().clone();
            let deadline = context.deadline();
            let count = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let check = || {
                    if cancellation.is_cancelled() { Err(provider_error(ProviderErrorKind::Cancelled, UpstreamSendState::NotSent)) }
                    else if deadline.is_elapsed() { Err(provider_error(ProviderErrorKind::Timeout, UpstreamSendState::NotSent)) }
                    else { Ok(()) }
                };
                check()?;
                let segments = if protocol == ApiKeyProtocol::Responses {
                    let translated = gateway_protocol::anthropic::messages_to_responses_count_request(&body).map_err(|_| invalid_request())?;
                    responses_segments(&translated)
                } else {
                    messages_segments(&body)
                };
                static O200K: OnceLock<Result<tiktoken_rs::CoreBPE, ()>> = OnceLock::new();
                static CL100K: OnceLock<Result<tiktoken_rs::CoreBPE, ()>> = OnceLock::new();
                // 与 CPA 的 Codex 映射一致，未知 Responses 模型使用 cl100k
                let modern = protocol == ApiKeyProtocol::Messages || ["gpt-5", "gpt-4.1", "gpt-4o"].iter().any(|prefix| tokenizer_model.starts_with(prefix));
                let encoder = if modern {
                    O200K.get_or_init(|| tiktoken_rs::o200k_base().map_err(|_| ()))
                } else {
                    CL100K.get_or_init(|| tiktoken_rs::cl100k_base().map_err(|_| ()))
                }.as_ref().map_err(|_| {
                    provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
                })?;
                check()?;
                Ok::<u64, ProviderError>(u64::try_from(encoder.encode_ordinary(&segments.join("\n")).len()).unwrap_or(u64::MAX))
            });
            let count = tokio::select! {
                biased;
                _ = context.cancellation().cancelled() => Err(provider_error(ProviderErrorKind::Cancelled, UpstreamSendState::NotSent)),
                _ = context.deadline().wait() => Err(provider_error(ProviderErrorKind::Timeout, UpstreamSendState::NotSent)),
                result = count => result.map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)).and_then(std::convert::identity),
            }?;
            let meta = ResponseMeta::new(context.request_id().as_str(), model.as_str());
            yield ProviderEvent::canonical(GatewayEvent::Started(meta.clone()));
            let wire = ProtocolWireEvent::raw_json(response_protocol, serde_json::to_vec(&serde_json::json!({"input_tokens":count})).map_err(|_| protocol_error())?.into()).map_err(|_| protocol_error())?;
            yield ProviderEvent::wire(wire).with_fact(GatewayEvent::Completed(meta.with_finish_reason(FinishReason::Stop)));
        });
        // 估算没有发送上游请求，不能计入账号响应成功率或上游用量
        Ok(ProviderStream::new(metadata, events, lease))
    }
}

fn push(segments: &mut Vec<String>, value: &Value) {
    if value.is_null() {
        return;
    }
    let text = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    let text = text.trim();
    if !text.is_empty() {
        segments.push(text.to_owned());
    }
}

fn fields(segments: &mut Vec<String>, value: &Value, names: &[&str]) {
    for name in names {
        push(segments, &value[name]);
    }
}

fn messages_segments(body: &Map<String, Value>) -> Vec<String> {
    let mut segments = Vec::new();
    if let Some(system) = body.get("system") {
        content_segments(system, &mut segments);
    }
    for message in body
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        push(&mut segments, &message["role"]);
        content_segments(&message["content"], &mut segments);
    }
    for tool in body
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        fields(
            &mut segments,
            tool,
            &["type", "name", "description", "input_schema"],
        );
    }
    if let Some(choice) = body.get("tool_choice") {
        if choice.is_string() {
            push(&mut segments, choice);
        } else {
            fields(&mut segments, choice, &["type", "name"]);
        }
    }
    segments
}

fn content_segments(content: &Value, segments: &mut Vec<String>) {
    if content.is_string() {
        push(segments, content);
        return;
    }
    if let Some(parts) = content.as_array() {
        for part in parts {
            content_segments(part, segments);
        }
        return;
    }
    if !content.is_object() {
        return;
    }
    match content
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "text" => push(segments, &content["text"]),
        "thinking" => push(segments, &content["thinking"]),
        "tool_use" | "server_tool_use" | "mcp_tool_use" => {
            fields(segments, content, &["id", "name", "input"])
        }
        kind if kind.ends_with("tool_result") => {
            fields(segments, content, &["tool_use_id", "tool_call_id"]);
            content_segments(&content["content"], segments);
        }
        "document" if content.pointer("/source/type").and_then(Value::as_str) == Some("text") => {
            fields(segments, content, &["title", "context"]);
            fields(segments, &content["source"], &["data", "content"]);
        }
        "web_search_result" | "search_result" => {
            fields(segments, content, &["source", "title", "url", "page_age"]);
            content_segments(&content["content"], segments);
        }
        "web_fetch_result" => {
            fields(segments, content, &["url", "retrieved_at"]);
            content_segments(&content["content"], segments);
        }
        "code_execution_result"
        | "bash_code_execution_result"
        | "text_editor_code_execution_result" => {
            fields(segments, content, &["stdout", "stderr", "return_code"]);
            content_segments(&content["content"], segments);
            content_segments(&content["output"], segments);
        }
        "tool_reference" => push(segments, &content["tool_name"]),
        "image" | "input_audio" | "audio" | "video" | "redacted_thinking" | "document" => {}
        "" => push(segments, content),
        _ => push(segments, &content["text"]),
    }
}

fn responses_segments(body: &Map<String, Value>) -> Vec<String> {
    let mut segments = Vec::new();
    if let Some(instructions) = body.get("instructions") {
        push(&mut segments, instructions);
    }
    for item in body
        .get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match item.get("type").and_then(Value::as_str) {
            Some("message") | None => {
                if item["content"].is_string() {
                    push(&mut segments, &item["content"]);
                }
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    push(&mut segments, &part["text"]);
                }
            }
            Some("function_call") => fields(&mut segments, item, &["name", "arguments"]),
            Some("function_call_output") => push(&mut segments, &item["output"]),
            _ => push(&mut segments, &item["text"]),
        }
    }
    for tool in body
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        fields(&mut segments, tool, &["name", "description", "parameters"]);
    }
    if let Some(format) = body.get("text").and_then(|value| value.get("format")) {
        fields(&mut segments, format, &["name", "schema"]);
    }
    segments
}
