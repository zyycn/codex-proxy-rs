//! OpenAI SSE 封装。
//!
//! 提供 SSE 事件编码与流式响应构建工具。

use axum::{
    body::Body,
    http::{
        StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};

use crate::upstream::openai::protocol::sse::DONE_SSE_FRAME;

use crate::api::client::errors::openai_error_response;

/// 编码 OpenAI 流结束标记。
pub fn done_sse_frame() -> &'static str {
    DONE_SSE_FRAME
}

pub fn event_stream_response(body: Body) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header(CACHE_CONTROL, "no-cache")
        .body(body)
        .unwrap_or_else(|_| {
            openai_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to build stream response",
                "server_error",
                "stream_response_error",
            )
            .into_response()
        })
}
