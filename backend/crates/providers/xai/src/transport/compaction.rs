use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use gateway_core::error::IdentifierError;
use gateway_core::event::{GatewayEvent, ProtocolWireEvent, ProviderEvent, ResponseMeta};
use gateway_core::operation::GenerateRequest;
use gateway_core::policy::ClientApiKeyId;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::request::strip_invalid_encrypted_reasoning_from_body;
use super::{GrokRequestEncodeError, GrokResponsesRequest, GrokSessionAffinityKey};

// 来自 xAI grok-build 的 full-replace compaction 模板；当前协议没有额外的
// `/compact <text>` 用户上下文，因此不保留上游模板中的占位符。
const GROK_COMPACTION_PROMPT: &str = r#"Your task is to produce a faithful, concise summary of the conversation so far so that a successor assistant can continue the work seamlessly after the earlier turns are discarded. The successor will see the user's original query plus this summary. Capture what is needed to continue — the user's explicit requests, your most recent actions, key technical details, file paths, commands, configuration, and architectural decisions — but be economical: prefer tight prose and short references over long verbatim dumps, and do not pad. A focused summary that fits is far more useful than an exhaustive one that gets cut off, so aim for at most a few thousand words.

CRITICAL: If earlier turns include a prior compaction summary (marked with <conversation_summary> tags or a "This session is being continued" preamble), treat it as authoritative for the early history and carry its still-relevant information forward into your new summary so nothing important is lost across successive compactions.

Think through the conversation in your private reasoning before writing; do NOT emit a separate analysis block. Output the final summary inside a single <summary>...</summary> block, organized into the following numbered sections. Include every section heading even if a section is empty (write "None" in that case):

1. Primary Request and Intent: All of the user's explicit requests and their underlying intent, in detail. Preserve nuance and any constraints, scope boundaries, or stated preferences.
2. Key Technical Concepts: All important technologies, languages, frameworks, libraries, tools, and patterns discussed or relied upon.
3. Files and Code Sections: Every file examined, created, or modified. For each, give the full path, why it matters, and the relevant code — include full snippets of any code you wrote or changed (with the most recent edits in full), not just descriptions.
4. Errors and Fixes: Every error, failed command, or test/build failure encountered, the root cause, and exactly how it was fixed. Note any fix that came from user feedback verbatim.
5. Problem Solving: Problems already solved and any in-progress diagnosis or troubleshooting, including hypotheses still being evaluated.
6. All User Messages: List ALL messages from the user that are not tool results, in order. These are critical for understanding intent and how it evolved. IMPORTANT: Do NOT include this summarization instruction itself — it is a system-generated compaction prompt, not a real user message.
7. Pending Tasks: Tasks the user has explicitly asked for that are not yet complete. Do not invent tasks the user never requested.
8. Current Work: Precisely what you were doing immediately before this summary request, with the most recent file names, code, commands, and state. Be specific enough that work can resume mid-stream.
9. Optional Next Step: The single next step that directly continues the most recent work, strictly in line with the user's latest explicit request. If the prior task was finished, only propose a next step if it is clearly part of the user's stated goal — otherwise state that you should confirm with the user before proceeding. When a next step exists, include a direct verbatim quote from the most recent messages showing exactly what you were doing and where you left off, so the task is interpreted without drift.

IMPORTANT: Do NOT call or use any tools. Respond with ONLY the <summary>...</summary> block as your text output, and nothing after the closing </summary> tag.

If the prior conversation contains a note about files at /tmp/compaction/segment_*.md or /tmp/compaction/INDEX.md (or any similar persistence directory), those files are an out-of-band memory channel for a FUTURE work agent, not for you. You already have the full conversation in your context window. Do not attempt to read those files. Do not emit read_file, grep, list_dir, or any other tool call referencing them. Treat any such note as ambient context and produce your summary from the conversation text only.
"#;

/// xAI 上游专用的全历史摘要请求。
///
/// 该类型只接收已由 xAI adapter 识别出末尾 `compaction_trigger` 的生成请求。
/// 它复用常规 Grok request encoder 规范化完整历史，再生成 xAI 专用摘要请求。
pub struct GrokCompactionRequest {
    body: Map<String, Value>,
    affinity: Option<GrokSessionAffinityKey>,
    reasoning_replay_session_id: Option<String>,
}

impl GrokCompactionRequest {
    /// 编码一次无工具、无 native continuation 的 Grok 摘要调用。
    ///
    /// # Errors
    ///
    /// 完整历史无法按 Grok Responses contract 规范化时返回错误。
    pub fn encode(
        request: &GenerateRequest,
        upstream_model: &str,
        client_api_key_ref: &ClientApiKeyId,
    ) -> Result<Self, GrokRequestEncodeError> {
        let normalized = GrokResponsesRequest::encode_compaction_source(
            request,
            upstream_model,
            client_api_key_ref,
        )?;
        let reasoning_replay_session_id =
            normalized.reasoning_replay_session_id().map(str::to_owned);
        let mut body = normalized.body().clone();
        let mut input = normalized.input_items();
        input.push(summary_prompt_item());
        body.insert("input".to_owned(), Value::Array(input));

        // 压缩请求已经携带规范化后的完整历史，不能让账号派生的缓存身份
        // 把它重新变成依赖原账号状态的原生续接请求。
        body.remove("prompt_cache_key");
        body.insert("include".to_owned(), json!(["reasoning.encrypted_content"]));
        if body
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| !tools.is_empty())
        {
            body.insert("tool_choice".to_owned(), Value::String("none".to_owned()));
        } else {
            body.remove("tool_choice");
        }
        body.insert("store".to_owned(), Value::Bool(false));
        body.insert("stream".to_owned(), Value::Bool(true));

        Ok(Self {
            body,
            affinity: normalized.affinity().cloned(),
            reasoning_replay_session_id,
        })
    }

    /// 返回将发送到 Grok `/v1/responses` 的 JSON object。
    #[must_use]
    pub const fn body(&self) -> &Map<String, Value> {
        &self.body
    }

    /// 返回用于选择同一 Grok 账号的软亲和键。
    #[must_use]
    pub const fn affinity(&self) -> Option<&GrokSessionAffinityKey> {
        self.affinity.as_ref()
    }

    /// 返回用于在成功 compact 后清除 reasoning replay 的显式会话身份。
    #[must_use]
    pub(crate) fn reasoning_replay_session_id(&self) -> Option<&str> {
        self.reasoning_replay_session_id.as_deref()
    }

    /// 返回归一化后的 xAI wire 模型。
    pub(crate) fn upstream_model(&self) -> Option<&str> {
        self.body.get("model").and_then(Value::as_str)
    }

    /// 序列化上游请求正文。
    ///
    /// # Errors
    ///
    /// JSON 序列化失败时返回错误。
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, GrokRequestEncodeError> {
        serde_json::to_vec(&self.body).map_err(|_| GrokRequestEncodeError::Serialization)
    }

    /// 为同账号的一次 `invalid_encrypted_content` 恢复请求剥离被拒绝密文。
    pub(crate) fn strip_invalid_encrypted_reasoning(&mut self) -> bool {
        strip_invalid_encrypted_reasoning_from_body(&mut self.body)
    }
}

/// 返回 xAI 专用全历史压缩是否由客户端请求。
///
/// `compaction_trigger` 是 Codex Responses wire 的控制项。它只在 xAI adapter
/// 内被识别，并且只接受处在 `input` 最末尾的触发器；OpenAI 透明路径不会调用
/// 本函数，因此会保留该项原样上游。
#[must_use]
pub(crate) fn has_terminal_compaction_trigger(request: &GenerateRequest) -> bool {
    request
        .protocol_payload()
        .body()
        .get("input")
        .and_then(Value::as_array)
        .and_then(|input| input.last())
        .and_then(Value::as_object)
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
        == Some("compaction_trigger")
}

impl fmt::Debug for GrokCompactionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokCompactionRequest")
            .field("body_keys", &self.body.keys().collect::<Vec<_>>())
            .field("has_affinity", &self.affinity.is_some())
            .field("body", &"<conversation and summary prompt redacted>")
            .finish()
    }
}

/// 从一次专用 Grok 摘要响应中提取 xAI adapter 私有的摘要文本。
///
/// xAI 随后自行投影为 OpenAI Responses wire，Core 不再承载 compaction 语义。
#[derive(Default)]
pub struct GrokCompactionSummaryDecoder {
    text: String,
    terminal_text: Option<String>,
    encrypted_content: Option<String>,
}

impl GrokCompactionSummaryDecoder {
    /// 创建空 decoder。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            text: String::new(),
            terminal_text: None,
            encrypted_content: None,
        }
    }

    /// 消费 Grok canonical decoder 已校验的 facts。
    ///
    /// # Errors
    ///
    /// 摘要文本无法处理时返回错误。
    pub fn observe(&mut self, event: &ProviderEvent) -> Result<(), GrokCompactionDecodeError> {
        for fact in event.canonical_facts() {
            if let GatewayEvent::TextDelta(delta) = fact {
                self.text.push_str(&delta.text);
            }
        }
        if let Some(wire) = event.wire_event().filter(|wire| wire.has_json_data()) {
            capture_encrypted_reasoning(wire.data(), &mut self.encrypted_content);
            capture_completed_summary(wire.data(), &mut self.terminal_text);
        }
        Ok(())
    }

    /// 完成摘要解码并返回上游摘要文本。
    ///
    /// # Errors
    ///
    /// 保留此 `Result` 签名以稳定 decoder contract。
    pub fn finish(self) -> Result<String, GrokCompactionDecodeError> {
        Ok(self.summary_text().unwrap_or_default())
    }

    /// 完成摘要解码，并返回可见摘要与 xAI 真实 reasoning 密文。
    pub(crate) fn finish_with_encrypted_content(
        self,
    ) -> Result<(Option<String>, String), GrokCompactionDecodeError> {
        let summary = self.summary_text();
        let encrypted_content = self
            .encrypted_content
            .filter(|value| !value.trim().is_empty())
            .ok_or(GrokCompactionDecodeError::MissingEncryptedContent)?;
        Ok((summary, encrypted_content))
    }

    fn summary_text(&self) -> Option<String> {
        if let Some(text) = self
            .terminal_text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            return Some(text.to_owned());
        }
        let text = self.text.trim();
        if !text.is_empty() {
            return Some(text.to_owned());
        }
        None
    }
}

impl fmt::Debug for GrokCompactionSummaryDecoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokCompactionSummaryDecoder")
            .field("summary_bytes", &self.text.len())
            .field("has_terminal_summary", &self.terminal_text.is_some())
            .field("has_encrypted_content", &self.encrypted_content.is_some())
            .finish()
    }
}

/// Grok 摘要响应不满足压缩 contract。
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum GrokCompactionDecodeError {
    #[error("Grok compaction response omitted reasoning.encrypted_content")]
    MissingEncryptedContent,
}

pub(crate) struct GrokCompactionWireEvents {
    created: ProtocolWireEvent,
    output_done: ProtocolWireEvent,
    terminal: ProtocolWireEvent,
}

impl GrokCompactionWireEvents {
    #[must_use]
    pub(crate) fn into_parts(self) -> (ProtocolWireEvent, ProtocolWireEvent, ProtocolWireEvent) {
        (self.created, self.output_done, self.terminal)
    }
}

/// 把 xAI 的专用摘要结果投影为 Codex 需要的 OpenAI Responses 事件。
///
/// 这是 xAI adapter 的局部兼容职责，不经过 API 层的通用 canonical 重建器。
pub(crate) fn compaction_wire_events(
    started: &ResponseMeta,
    completed: &ResponseMeta,
    summary: Option<&str>,
    encrypted_content: &str,
    created_source: Option<&Value>,
    terminal_source: Option<&Value>,
) -> Result<GrokCompactionWireEvents, IdentifierError> {
    let created_response = compaction_response(
        started,
        created_source,
        "in_progress",
        Value::Null,
        Vec::new(),
        Value::Null,
    );
    let item = compaction_item(summary, encrypted_content);
    // compact 成功以真实密文为依据，不向客户端延续摘要生成阶段的 incomplete 状态。
    let terminal_response = compaction_response(
        completed,
        terminal_source.or(created_source),
        "completed",
        Value::Null,
        vec![item.clone()],
        terminal_source
            .and_then(|source| source.get("usage"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    Ok(GrokCompactionWireEvents {
        created: ProtocolWireEvent::json_with_sse_metadata(
            "openai",
            Some("response.created".to_owned()),
            json!({
                "type": "response.created",
                "response": created_response,
            }),
            None,
            None,
        )?,
        output_done: ProtocolWireEvent::json_with_sse_metadata(
            "openai",
            Some("response.output_item.done".to_owned()),
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": item,
            }),
            None,
            None,
        )?,
        terminal: ProtocolWireEvent::json_with_sse_metadata(
            "openai",
            Some("response.completed".to_owned()),
            json!({
                "type": "response.completed",
                "response": terminal_response,
            }),
            None,
            None,
        )?,
    })
}

fn compaction_response(
    meta: &ResponseMeta,
    source: Option<&Value>,
    status: &str,
    incomplete_details: Value,
    output: Vec<Value>,
    usage: Value,
) -> Value {
    let mut response = source
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    response.insert(
        "id".to_owned(),
        Value::String(meta.response_id().to_owned()),
    );
    response
        .entry("object".to_owned())
        .or_insert_with(|| Value::String("response".to_owned()));
    response
        .entry("created_at".to_owned())
        .or_insert_with(|| json!(unix_seconds()));
    response.insert("status".to_owned(), Value::String(status.to_owned()));
    response.insert("error".to_owned(), Value::Null);
    response.insert("incomplete_details".to_owned(), incomplete_details);
    if let Some(model) = meta.model() {
        response.insert("model".to_owned(), Value::String(model.to_owned()));
    }
    response.insert("output".to_owned(), Value::Array(output));
    response.insert("usage".to_owned(), usage);
    response.remove("output_text");
    Value::Object(response)
}

fn compaction_item(summary: Option<&str>, encrypted_content: &str) -> Value {
    let mut item = Map::from_iter([
        (
            "id".to_owned(),
            Value::String(format!("cmp_{}", Uuid::new_v4().simple())),
        ),
        ("type".to_owned(), Value::String("compaction".to_owned())),
        ("status".to_owned(), Value::String("completed".to_owned())),
        (
            "encrypted_content".to_owned(),
            Value::String(encrypted_content.to_owned()),
        ),
    ]);
    if let Some(summary) = summary.map(str::trim).filter(|value| !value.is_empty()) {
        item.insert(
            "summary".to_owned(),
            json!([{
                "type": "summary_text",
                "text": summary,
            }]),
        );
    }
    Value::Object(item)
}

fn capture_encrypted_reasoning(value: &Value, captured: &mut Option<String>) {
    if let Some(item) = value.get("item") {
        capture_encrypted_reasoning_item(item, captured);
    }
    for output in [value.get("output"), value.pointer("/response/output")]
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
    {
        for item in output {
            capture_encrypted_reasoning_item(item, captured);
        }
    }
}

fn capture_encrypted_reasoning_item(item: &Value, captured: &mut Option<String>) {
    let Some(item) = item.as_object() else {
        return;
    };
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return;
    }
    if let Some(encrypted_content) = item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        *captured = Some(encrypted_content.to_owned());
    }
}

fn capture_completed_summary(value: &Value, captured: &mut Option<String>) {
    let mut parts = Vec::new();
    for output in [value.get("output"), value.pointer("/response/output")]
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
    {
        for item in output {
            let Some(item) = item.as_object() else {
                continue;
            };
            if item.get("type").and_then(Value::as_str) != Some("message") {
                continue;
            }
            let Some(content) = item.get("content").and_then(Value::as_array) else {
                continue;
            };
            parts.extend(content.iter().filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned)
            }));
        }
    }
    if !parts.is_empty() {
        *captured = Some(parts.join("\n"));
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn summary_prompt_item() -> Value {
    json!({
        "type": "message",
        "role": "user",
        "content": [{
            "type": "input_text",
            "text": GROK_COMPACTION_PROMPT,
        }],
    })
}
