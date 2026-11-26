//! AGY 编码请求保留函数工具；只有强制搜索才构造独立的原生请求

use crate::transport::sent;
use gateway_core::{
    engine::provider::MAX_NATIVE_RESPONSE_EVENTS_PER_INPUT,
    error::{ProviderError, ProviderErrorKind},
    event::ProtocolWireEvent,
};
use gateway_protocol::{
    anthropic::MessagesToResponsesStream,
    genai::{GenAiToMessagesStream, grounded_search_events},
    openai::{OPENAI_PROTOCOL, web_search},
};
use serde_json::{Map, Value, json};
use std::borrow::Cow;

pub(super) fn request(
    original: &Map<String, Value>,
    tool: &Value,
) -> Result<Map<String, Value>, ProviderError> {
    if !web_search::allows_live_access(tool) {
        return Err(ProviderError::invalid_request(
            "Antigravity web search requires live external web access.",
        ));
    }
    let options = google_search_options(tool)?;
    if original
        .get("text")
        .and_then(|v| v.pointer("/format/type"))
        .is_some_and(|v| v != "text")
    {
        return Err(ProviderError::invalid_request(
            "Antigravity web search supports text output only.",
        ));
    }
    // Desktop 的独立搜索只提交当前用户查询，编码历史和函数返回不进入搜索供应方
    let input = original.get("input").unwrap_or(&Value::Null);
    let query = if let Some(items) = input.as_array() {
        let item = items
            .last()
            .filter(|i| i["role"] == "user")
            .ok_or_else(invalid_query)?;
        let content = &item["content"];
        if let Some(parts) = content.as_array() {
            parts
                .iter()
                .map(|p| {
                    (p["type"] == "input_text")
                        .then(|| p["text"].as_str())
                        .flatten()
                        .ok_or_else(invalid_query)
                })
                .collect::<Result<Vec<_>, _>>()?
                .join("\n")
        } else {
            content.as_str().ok_or_else(invalid_query)?.to_owned()
        }
    } else {
        input.as_str().ok_or_else(invalid_query)?.to_owned()
    };
    if query.trim().is_empty() {
        return Err(invalid_query());
    }
    let mut body = Map::from_iter([
        (
            "contents".into(),
            json!([{"role":"user","parts":[{"text":query}]}]),
        ),
        ("tools".into(), json!([{"googleSearch":options}])),
    ]);
    if let Some(limit) = original.get("max_output_tokens").filter(|v| !v.is_null()) {
        let limit = limit.as_u64().filter(|n| *n > 0).ok_or_else(|| {
            ProviderError::invalid_request("max_output_tokens must be a positive integer.")
        })?;
        body.insert("generationConfig".into(), json!({"maxOutputTokens":limit}));
    }
    Ok(body)
}

fn google_search_options(tool: &Value) -> Result<Map<String, Value>, ProviderError> {
    let unsupported = || {
        ProviderError::invalid_request("Antigravity cannot honor the specified web search options.")
    };
    let tool = tool.as_object().ok_or_else(unsupported)?;
    if tool.iter().any(|(k, v)| {
        !v.is_null() && !matches!(k.as_str(), "type" | "external_web_access" | "filters")
    }) {
        return Err(unsupported());
    }
    let mut options = Map::new();
    if let Some(filters) = tool.get("filters").filter(|v| !v.is_null()) {
        for (field, domains) in filters.as_object().ok_or_else(unsupported)? {
            let field = match field.as_str() {
                "allowed_domains" => "includedDomains",
                "blocked_domains" => "excludeDomains",
                _ => return Err(unsupported()),
            };
            let domains = domains
                .as_array()
                .filter(|values| {
                    !values.is_empty()
                        && values
                            .iter()
                            .all(|v| v.as_str().is_some_and(|s| !s.trim().is_empty()))
                })
                .ok_or_else(unsupported)?;
            options.insert(field.into(), json!(domains));
        }
    }
    Ok(options)
}

fn invalid_query() -> ProviderError {
    ProviderError::invalid_request(
        "Antigravity web search requires a text query in the last user message.",
    )
}

pub(super) struct SearchResponse {
    responses: MessagesToResponsesStream,
    include_sources: bool,
    terminal: Option<ProtocolWireEvent>,
}

impl SearchResponse {
    pub(super) fn new(original: &Map<String, Value>) -> Self {
        Self {
            responses: MessagesToResponsesStream::new(original),
            include_sources: original
                .get("include")
                .and_then(Value::as_array)
                .is_some_and(|v| v.iter().any(|v| v == "web_search_call.action.sources")),
            terminal: None,
        }
    }

    pub(super) fn translate(
        &mut self,
        event: &ProtocolWireEvent,
    ) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        let value = match event.raw_json_body() {
            Some(raw) => Cow::Owned(
                serde_json::from_slice(raw).map_err(|_| sent(ProviderErrorKind::Protocol))?,
            ),
            None => Cow::Borrowed(event.data()),
        };
        let error = |e| sent(ProviderErrorKind::Protocol).with_source(e);
        let mut genai = GenAiToMessagesStream::new(String::new());
        let mut messages = genai.push(&value).map_err(error)?;
        messages.extend(genai.finish().map_err(error)?);
        let mut responses = Vec::new();
        for message in messages {
            responses.extend(self.responses.push(&message).map_err(error)?);
        }
        let mut wire: Vec<_> = grounded_search_events(
            &value,
            responses,
            self.include_sources,
            MAX_NATIVE_RESPONSE_EVENTS_PER_INPUT + 1,
        )
        .map_err(error)?
        .into_iter()
        .map(|event| {
            ProtocolWireEvent::json(
                OPENAI_PROTOCOL,
                event["type"].as_str().map(str::to_owned),
                event,
            )
            .map_err(|_| sent(ProviderErrorKind::Protocol))
        })
        .collect::<Result<_, _>>()?;
        // 单次 JSON 先交付内容；协议终态必须随 Core 的 Completed 事实交付
        self.terminal = wire.pop();
        Ok(wire)
    }

    pub(super) fn finish(&mut self) -> Result<Vec<ProtocolWireEvent>, ProviderError> {
        self.terminal
            .take()
            .map(|v| vec![v])
            .ok_or_else(|| sent(ProviderErrorKind::Protocol))
    }
}
