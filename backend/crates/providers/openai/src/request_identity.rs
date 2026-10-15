//! 请求正文与最终头部的会话身份解释，供账号亲和与传输编码共用

use std::borrow::Cow;

use serde_json::{Map, Value};

/// 只有能识别出的后代线程禁止迁移会话；没有身份的普通客户端沿用现有调度
pub(crate) fn follows_session_with_headers(
    body: &Map<String, Value>,
    context: &Map<String, Value>,
    headers: &[gateway_core::engine::middleware::MiddlewareHeader],
) -> bool {
    let metadata = headers
        .iter()
        .find(|h| h.name().eq_ignore_ascii_case("x-codex-turn-metadata"))
        .and_then(|h| std::str::from_utf8(h.value()).ok());
    let thread = account_thread_with_headers(body, context, headers);
    if let Some((session, thread)) =
        account_session_with_headers(body, context, headers).zip(thread)
    {
        return session != thread;
    }
    gateway_protocol::openai::codex_responses_request_semantics_with_turn_metadata(
        body,
        metadata.or_else(|| context.get("turn_metadata").and_then(Value::as_str)),
    )
    .subagent_kind
    .is_some()
}

fn account_thread_with_headers(
    body: &Map<String, Value>,
    context: &Map<String, Value>,
    headers: &[gateway_core::engine::middleware::MiddlewareHeader],
) -> Option<String> {
    let header = |name: &str| {
        headers
            .iter()
            .find(|h| h.name().eq_ignore_ascii_case(name))
            .and_then(|h| std::str::from_utf8(h.value()).ok())
    };
    header("x-codex-turn-metadata")
        .and_then(gateway_protocol::openai::turn_metadata_thread_id)
        .or_else(|| {
            header("thread-id")
                .and_then(|value| non_empty(Some(value)))
                .map(str::to_owned)
        })
        .or_else(|| gateway_protocol::openai::codex_account_thread_id(body, context))
}

/// 中间件最终请求头按实际传输的覆盖语义参与身份复验
pub(crate) fn account_session_with_headers(
    body: &Map<String, Value>,
    context: &Map<String, Value>,
    headers: &[gateway_core::engine::middleware::MiddlewareHeader],
) -> Option<String> {
    let header = |name: &str| {
        headers
            .iter()
            .find(|h| h.name().eq_ignore_ascii_case(name))
            .and_then(|h| std::str::from_utf8(h.value()).ok())
    };
    if let Some(session) =
        header("x-codex-turn-metadata").and_then(gateway_protocol::openai::turn_metadata_session_id)
    {
        return Some(session);
    }
    let mut context = Cow::Borrowed(context);
    for (name, field) in [
        ("session-id", "session_id"),
        ("x-codex-turn-metadata", "turn_metadata"),
    ] {
        if let Some(value) = header(name) {
            context
                .to_mut()
                .insert(field.to_owned(), Value::String(value.to_owned()));
        }
    }
    gateway_protocol::openai::codex_account_session_id(body, &context)
}

pub(crate) fn turn_id_with_headers(
    body: &Map<String, Value>,
    context: &Map<String, Value>,
    headers: &[gateway_core::engine::middleware::MiddlewareHeader],
) -> Option<String> {
    headers
        .iter()
        .find(|header| header.name().eq_ignore_ascii_case("x-codex-turn-metadata"))
        .and_then(|header| std::str::from_utf8(header.value()).ok())
        .and_then(gateway_protocol::openai::turn_metadata_turn_id)
        .or_else(|| {
            headers
                .iter()
                .find(|header| header.name().eq_ignore_ascii_case("x-client-turn-id"))
                .and_then(|header| std::str::from_utf8(header.value()).ok())
                .and_then(|value| non_empty(Some(value)))
                .map(str::to_owned)
        })
        .or_else(|| gateway_protocol::openai::codex_turn_id(body, context))
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
