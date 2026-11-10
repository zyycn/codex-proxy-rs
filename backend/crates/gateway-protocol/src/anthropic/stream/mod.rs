//! 每次执行独立持有的 SSE 聚合与转换状态

mod accumulator;
mod to_messages;
mod to_responses;

pub use accumulator::MessagesAccumulator;
pub use to_messages::ResponsesToMessagesStream;
pub use to_responses::MessagesToResponsesStream;

use super::TranslationError;
use serde_json::{Value, json};

/// 将已收到的完整原生消息交付给请求 SSE 的客户端，保留内容块与扩展字段
///
/// # Errors
///
/// 对象不是完整 Messages 响应或缺少内容数组时返回错误
pub fn message_to_stream_events(mut message: Value) -> Result<Vec<Value>, TranslationError> {
    if message.get("type").and_then(Value::as_str) != Some("message") {
        return Err(TranslationError::InvalidEvent(
            "The upstream response is not a message.",
        ));
    }
    let content = message
        .get_mut("content")
        .and_then(Value::as_array_mut)
        .map(std::mem::take)
        .ok_or(TranslationError::InvalidEvent(
            "The upstream message content is missing.",
        ))?;
    let stop_reason = message["stop_reason"].take();
    let stop_sequence = message["stop_sequence"].take();
    let usage = message.get("usage").cloned().unwrap_or_else(|| json!({}));
    let mut events = vec![json!({"type":"message_start","message":message})];
    for (index, block) in content.into_iter().enumerate() {
        events.push(json!({"type":"content_block_start","index":index,"content_block":block}));
        events.push(json!({"type":"content_block_stop","index":index}));
    }
    events.push(json!({"type":"message_delta","delta":{"stop_reason":stop_reason,"stop_sequence":stop_sequence},"usage":usage}));
    events.push(json!({"type":"message_stop"}));
    Ok(events)
}
