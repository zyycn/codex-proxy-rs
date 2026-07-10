//! OpenAI SSE 封装。
//!
//! 提供 SSE 事件编码与流式响应构建工具。

use axum::{
    body::Body,
    http::{
        header::{CACHE_CONTROL, CONTENT_TYPE},
        StatusCode,
    },
    response::{IntoResponse, Response},
};

use crate::upstream::openai::protocol::sse::{encode_sse_event, DONE_SSE_FRAME};

use super::errors::openai_error_response;

#[derive(Clone, Copy)]
pub struct SseResponseOptions {
    pub keep_alive: bool,
    pub disable_accel_buffering: bool,
}

impl SseResponseOptions {
    pub const BASIC: Self = Self {
        keep_alive: false,
        disable_accel_buffering: false,
    };

    pub const CHAT_ERROR: Self = Self {
        keep_alive: true,
        disable_accel_buffering: false,
    };

    pub const LIVE_CHAT: Self = Self {
        keep_alive: true,
        disable_accel_buffering: true,
    };
}

/// 编码 OpenAI 兼容 SSE 事件帧。
pub fn openai_sse_frame(event: &str, data: &str) -> String {
    encode_sse_event(event, data)
}

/// 编码 OpenAI 流结束标记。
pub fn done_sse_frame() -> &'static str {
    DONE_SSE_FRAME
}

pub fn event_stream_response(body: Body, options: SseResponseOptions) -> Response {
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header(CACHE_CONTROL, "no-cache");

    if options.keep_alive {
        builder = builder.header("connection", "keep-alive");
    }
    if options.disable_accel_buffering {
        builder = builder.header("x-accel-buffering", "no");
    }

    builder.body(body).unwrap_or_else(|_| {
        openai_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to build stream response",
            "server_error",
            "stream_response_error",
        )
        .into_response()
    })
}
