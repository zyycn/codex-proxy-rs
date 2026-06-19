//! OpenAI SSE 封装。

use codex_proxy_core::protocol::codex::sse::encode_sse_event;

/// 编码 OpenAI 兼容 SSE 事件帧。
pub fn openai_sse_frame(event: &str, data: &str) -> String {
    encode_sse_event(event, data)
}

/// 编码 OpenAI 流结束标记。
pub fn done_sse_frame() -> &'static str {
    "data: [DONE]\n\n"
}
