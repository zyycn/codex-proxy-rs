use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use gateway_core::error::IdentifierError;
use gateway_core::event::{
    FinishReason, GatewayEvent, ProtocolWireEvent, ProviderEvent, ResponseMeta,
};
use gateway_core::operation::GenerateRequest;
use gateway_core::policy::ClientApiKeyId;
use serde_json::{Map, Value, json};

use super::{GrokRequestEncodeError, GrokResponsesRequest, GrokSessionAffinityKey};

const MIN_GROK_COMPACTION_SUMMARY_CHARS: usize = 500;
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

        for field in [
            "background",
            "include",
            "max_output_tokens",
            "parallel_tool_calls",
            "previous_response_id",
            "prompt_cache_key",
            "response_format",
            "service_tier",
            "text",
            "truncation",
        ] {
            body.remove(field);
        }
        body.insert("temperature".to_owned(), json!(1.0));
        if body
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| !tools.is_empty())
        {
            body.insert("tool_choice".to_owned(), Value::String("auto".to_owned()));
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

    /// 序列化上游请求正文。
    ///
    /// # Errors
    ///
    /// JSON 序列化失败时返回错误。
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, GrokRequestEncodeError> {
        serde_json::to_vec(&self.body).map_err(|_| GrokRequestEncodeError::Serialization)
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
}

impl GrokCompactionSummaryDecoder {
    /// 创建空 decoder。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            text: String::new(),
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
        Ok(())
    }

    /// 完成摘要解码并返回清理后的摘要文本。
    ///
    /// # Errors
    ///
    /// 摘要为空或过短时返回错误。
    pub fn finish(self) -> Result<String, GrokCompactionDecodeError> {
        let summary = clean_summary(&self.text);
        if summary.is_empty() || summary.chars().count() < MIN_GROK_COMPACTION_SUMMARY_CHARS {
            return Err(GrokCompactionDecodeError::Degenerate);
        }
        Ok(summary)
    }
}

impl fmt::Debug for GrokCompactionSummaryDecoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokCompactionSummaryDecoder")
            .field("summary_bytes", &self.text.len())
            .finish()
    }
}

/// Grok 摘要响应不满足压缩 contract。
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum GrokCompactionDecodeError {
    #[error("Grok compaction response summary was too short")]
    Degenerate,
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
    summary: &str,
    created_source: Option<&Value>,
    terminal_source: Option<&Value>,
    terminal_was_incomplete: bool,
) -> Result<GrokCompactionWireEvents, IdentifierError> {
    let created_response = compaction_response(
        started,
        created_source,
        "in_progress",
        Value::Null,
        Vec::new(),
        Value::Null,
    );
    let item = compaction_item(summary);
    let (status, terminal_event, incomplete_details) = terminal_shape(
        completed.finish_reason(),
        terminal_was_incomplete,
        terminal_source,
    );
    let terminal_response = compaction_response(
        completed,
        terminal_source.or(created_source),
        status,
        incomplete_details,
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
            Some(terminal_event.to_owned()),
            json!({
                "type": terminal_event,
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
    response.insert("model".to_owned(), Value::String(meta.model().to_owned()));
    response.insert("output".to_owned(), Value::Array(output));
    response.insert("usage".to_owned(), usage);
    Value::Object(response)
}

fn compaction_item(summary: &str) -> Value {
    json!({
        "type": "compaction",
        "encrypted_content": summary,
    })
}

fn terminal_shape(
    finish_reason: Option<FinishReason>,
    terminal_was_incomplete: bool,
    terminal_source: Option<&Value>,
) -> (&'static str, &'static str, Value) {
    let incomplete = terminal_was_incomplete
        || matches!(
            finish_reason,
            Some(FinishReason::Length | FinishReason::ContentFilter)
        );
    if !incomplete {
        return ("completed", "response.completed", Value::Null);
    }
    let details = terminal_source
        .and_then(|source| source.get("incomplete_details"))
        .cloned()
        .unwrap_or_else(|| match finish_reason {
            Some(FinishReason::Length) => json!({ "reason": "max_output_tokens" }),
            Some(FinishReason::ContentFilter) => json!({ "reason": "content_filter" }),
            _ => json!({ "reason": "other" }),
        });
    ("incomplete", "response.incomplete", details)
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

fn clean_summary(raw: &str) -> String {
    let mut summary = raw.to_owned();
    while let Some(start) = summary.find("<analysis>") {
        let leading = match summary.find("<summary>") {
            Some(summary_start) => {
                start < summary_start
                    || summary[summary_start + "<summary>".len()..start]
                        .trim()
                        .is_empty()
            }
            None => summary[..start].trim().is_empty(),
        };
        if !leading {
            break;
        }
        if let Some(relative_end) = summary[start..].find("</analysis>") {
            let end = start + relative_end + "</analysis>".len();
            summary.replace_range(start..end, "");
        } else {
            let drop_end = summary[start..]
                .find("<summary>")
                .map_or(summary.len(), |relative| start + relative);
            summary.replace_range(start..drop_end, "");
            break;
        }
    }

    if let Some(start) = summary.find("<summary>")
        && let Some(end) = summary.rfind("</summary>")
        && end > start
    {
        let before = summary[..start].to_owned();
        let after = summary[end + "</summary>".len()..].to_owned();
        let inner = strip_leading_scratchpad(summary[start + "<summary>".len()..end].trim());
        summary = format!("{before}Summary:\n{inner}{after}");
    }

    summary = neutralize_compaction_control_tokens(&summary);
    while summary.contains("\n\n\n") {
        summary = summary.replace("\n\n\n", "\n\n");
    }
    summary.trim().to_owned()
}

fn strip_leading_scratchpad(inner: &str) -> String {
    let mut summary = inner.trim();
    let lead = summary.trim_start_matches(['#', '*', '-', '>', ' ', '\t']);
    if !lead.starts_with(|character: char| character.is_ascii_digit())
        && let Some(end) = summary.rfind("</analysis>")
    {
        summary = summary[end + "</analysis>".len()..].trim_start();
    }
    if let Some(inner) = summary.strip_prefix("<summary>") {
        summary = inner.trim_start();
    }
    summary.to_owned()
}

fn neutralize_compaction_control_tokens(summary: &str) -> String {
    summary
        .replace("</summary>", "<\u{200b}/summary>")
        .replace("<summary>", "<\u{200b}summary>")
        .replace("</analysis>", "<\u{200b}/analysis>")
        .replace("<analysis>", "<\u{200b}analysis>")
        .replace("</summary_request>", "<\u{200b}/summary_request>")
        .replace("<summary_request>", "<\u{200b}summary_request>")
}
