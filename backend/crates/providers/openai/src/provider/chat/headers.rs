//! Chat 到 Messages 的协议头隔离，不改变其他原生入口

use gateway_core::engine::middleware::MiddlewareHeader;
use serde_json::{Map, Value};

pub(super) fn retain_messages_headers(
    context: &mut Map<String, Value>,
    headers: &mut Vec<MiddlewareHeader>,
) {
    let keep = |name: &str| {
        let name = name.to_ascii_lowercase();
        ![
            "openai-",
            "x-openai-",
            "x-codex-",
            "x-oai-",
            "x-responsesapi-",
            "chatgpt-",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
            && !matches!(
                name.as_str(),
                "session-id" | "conversation-id" | "thread-id" | "turn-id" | "session_id"
            )
    };
    if let Some(entries) = context
        .get_mut(crate::transport::request::PASSTHROUGH_HEADERS_CONTEXT_KEY)
        .and_then(Value::as_array_mut)
    {
        entries.retain(|entry| {
            entry
                .as_array()
                .and_then(|entry| entry.first())
                .and_then(Value::as_str)
                .is_some_and(keep)
        });
    }
    headers.retain(|header| keep(header.name()));
}
