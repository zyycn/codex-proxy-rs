//! SSE 事件解析与编码。

use std::fmt;

use serde_json::json;
use thiserror::Error;

/// 单条 SSE 事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// 事件名。
    pub event: Option<String>,
    /// 数据体。
    pub data: String,
    /// 可选 ID。
    pub id: Option<String>,
    /// 可选 retry。
    pub retry: Option<u64>,
}

/// 保留完整原始字节与旁路解析结果的单个 SSE 帧。
///
/// 这用于需要原样转发的 transport；`events` 只供旁路观测，不能反过来重建
/// `raw`。
#[derive(Clone, PartialEq, Eq)]
pub struct SseFrame {
    raw: Vec<u8>,
    events: Vec<SseEvent>,
}

impl SseFrame {
    fn new(raw: Vec<u8>, events: Vec<SseEvent>) -> Self {
        Self { raw, events }
    }

    /// 返回未经改写的完整 SSE 帧字节。
    #[must_use]
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// 返回从同一原始帧旁路解析出的事件。
    #[must_use]
    pub fn events(&self) -> &[SseEvent] {
        &self.events
    }

    /// 拆出原始帧与旁路解析事件。
    #[must_use]
    pub fn into_parts(self) -> (Vec<u8>, Vec<SseEvent>) {
        (self.raw, self.events)
    }
}

impl fmt::Debug for SseFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SseFrame")
            .field("raw_len", &self.raw.len())
            .field("events", &self.events)
            .finish()
    }
}

/// 单事件缓冲上限。
pub const MAX_SSE_EVENT_BUFFER_BYTES: usize = 64 * 1024 * 1024;

/// SSE 流结束标记帧。
pub const DONE_SSE_FRAME: &str = "data: [DONE]\n\n";

/// 用于处理任意分块边界的增量 SSE 解码器。
#[derive(Debug, Default)]
pub struct SseEventDecoder {
    pending: Vec<u8>,
    /// `pending` 中已确认不含帧分隔符的前缀长度。新数据到达时从该边界
    /// 回退分隔符最大跨界宽度继续扫描，大帧跨多个 chunk 不会被反复重扫。
    scanned: usize,
}

impl SseEventDecoder {
    /// 返回从 `consumed` 起的下一个完整帧结束位置（含分隔符），并推进扫描边界。
    fn next_frame_end(&mut self, consumed: usize) -> Option<usize> {
        // `\r\n\r\n` 分隔符可能跨 chunk 边界，扫描起点回退 3 字节。
        let from = self.scanned.saturating_sub(3).max(consumed);
        match sse_frame_separator_bytes(&self.pending[from..]) {
            Some((position, separator_len)) => {
                let end = from + position + separator_len;
                self.scanned = end;
                Some(end)
            }
            None => {
                self.scanned = self.pending.len();
                None
            }
        }
    }

    fn commit_consumed(&mut self, consumed: usize) {
        if consumed != 0 {
            self.pending.drain(..consumed);
            self.scanned = self.scanned.saturating_sub(consumed);
        }
    }

    /// 追加一个字节块并返回其中已经完整的事件。
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, SseError> {
        self.pending.extend_from_slice(chunk);
        let mut events = Vec::new();
        let mut consumed = 0usize;

        while let Some(end) = self.next_frame_end(consumed) {
            let frame = std::str::from_utf8(&self.pending[consumed..end])
                .map_err(|error| SseError::ParseError(error.to_string()))?;
            events.extend(parse_sse_events(frame)?);
            consumed = end;
        }

        self.commit_consumed(consumed);
        if self.pending.len() > MAX_SSE_EVENT_BUFFER_BYTES {
            return Err(SseError::BufferExceeded {
                max_bytes: MAX_SSE_EVENT_BUFFER_BYTES,
            });
        }
        Ok(events)
    }

    /// 追加一个字节块，保留每个完整 SSE 帧的原始字节及其旁路解析结果。
    ///
    /// 与 [`Self::push`] 相比，此方法仅应由需要原样转发的 transport 使用；普通
    /// 解析路径无需为原始帧额外分配内存。旁路解析失败时仍保留原始帧，并以空
    /// event 列表表示不可观测，避免观测失败截断客户端字节流。
    pub fn push_frames(&mut self, chunk: &[u8]) -> Result<Vec<SseFrame>, SseError> {
        self.pending.extend_from_slice(chunk);
        let mut frames = Vec::new();
        let mut consumed = 0usize;

        while let Some(end) = self.next_frame_end(consumed) {
            let raw = self.pending[consumed..end].to_vec();
            let events = std::str::from_utf8(&raw)
                .ok()
                .and_then(|frame| parse_sse_events(frame).ok())
                .unwrap_or_default();
            frames.push(SseFrame::new(raw, events));
            consumed = end;
        }

        self.commit_consumed(consumed);
        if self.pending.len() > MAX_SSE_EVENT_BUFFER_BYTES {
            return Err(SseError::BufferExceeded {
                max_bytes: MAX_SSE_EVENT_BUFFER_BYTES,
            });
        }
        Ok(frames)
    }

    /// 流结束时解析尚未带空行分隔符的最后一帧。
    pub fn finish(&mut self) -> Result<Vec<SseEvent>, SseError> {
        self.scanned = 0;
        if self.pending.is_empty() {
            return Ok(Vec::new());
        }
        let pending = std::mem::take(&mut self.pending);
        let frame = std::str::from_utf8(&pending)
            .map_err(|error| SseError::ParseError(error.to_string()))?;
        parse_sse_events(frame)
    }

    /// 在流结束时返回未带空行分隔符的最后一帧及其旁路解析结果。
    ///
    /// 与 [`Self::push_frames`] 一致，旁路解析失败不会丢弃原始帧。
    pub fn finish_frames(&mut self) -> Result<Vec<SseFrame>, SseError> {
        self.scanned = 0;
        if self.pending.is_empty() {
            return Ok(Vec::new());
        }
        let raw = std::mem::take(&mut self.pending);
        let events = std::str::from_utf8(&raw)
            .ok()
            .and_then(|frame| parse_sse_events(frame).ok())
            .unwrap_or_default();
        Ok(vec![SseFrame::new(raw, events)])
    }
}

/// 编码 OpenAI Responses `response.failed` SSE 事件。
pub fn response_failed_sse_event(error_type: &str, code: &str, message: &str) -> String {
    response_failed_sse_event_with_id(None, error_type, code, message)
}

/// 使用指定 response id 编码 OpenAI Responses `response.failed` SSE 事件。
pub fn response_failed_sse_event_with_id(
    response_id: Option<&str>,
    error_type: &str,
    code: &str,
    message: &str,
) -> String {
    let data = response_failed_sse_data_with_id(response_id, error_type, code, message);
    encode_sse_event("response.failed", &data.to_string())
}

/// 构造 OpenAI Responses `response.failed` 的 JSON 数据。
pub fn response_failed_sse_data_with_id(
    response_id: Option<&str>,
    error_type: &str,
    code: &str,
    message: &str,
) -> serde_json::Value {
    let error = json!({
        "type": error_type,
        "code": code,
        "message": message,
    });
    let response_id = response_id
        .filter(|value| !value.trim().is_empty())
        .map_or_else(
            || format!("resp_proxy_{}", uuid::Uuid::new_v4().simple()),
            ToString::to_string,
        );
    json!({
        "type": "response.failed",
        "response": {
            "id": response_id,
            "status": "failed",
            "error": error,
        },
        "error": error,
    })
}

/// 返回下一个完整 SSE 帧结束位置（含分隔符）。
pub fn sse_frame_end(bytes: &[u8]) -> Option<usize> {
    sse_frame_separator_bytes(bytes).map(|(position, separator_len)| position + separator_len)
}

/// 判断完整 SSE 帧是否是单个 `[DONE]` 控制帧。
#[must_use]
pub fn sse_frame_is_done(frame: &str) -> bool {
    let mut data = frame.lines().filter_map(|raw_line| {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let (field, value) = split_sse_field(line);
        (field == "data").then_some(value)
    });
    matches!((data.next(), data.next()), (Some("[DONE]"), None))
}

/// SSE 解析错误。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SseError {
    /// retry 字段不是合法整数。
    #[error("invalid SSE retry value: {0}")]
    InvalidRetry(String),
    /// 单个事件缓冲超过上限。
    #[error("SSE buffer exceeded {max_bytes} bytes — aborting stream")]
    BufferExceeded {
        /// 上限字节数。
        max_bytes: usize,
    },
    /// 解析错误。
    #[error("SSE parse error: {0}")]
    ParseError(String),
}

#[derive(Debug, Default)]
struct EventBuilder {
    event: Option<String>,
    data: String,
    has_data: bool,
    id: Option<String>,
    retry: Option<u64>,
}

impl EventBuilder {
    fn push_data(&mut self, value: &str) {
        if self.has_data {
            self.data.push('\n');
        }
        self.data.push_str(value);
        self.has_data = true;
    }

    fn finish(&mut self) -> Option<SseEvent> {
        if !self.has_data {
            self.event = None;
            self.id = None;
            self.retry = None;
            return None;
        }
        self.has_data = false;
        if self.data == "[DONE]" {
            self.event = None;
            self.id = None;
            self.retry = None;
            self.data.clear();
            return None;
        }
        Some(SseEvent {
            event: self.event.take(),
            data: std::mem::take(&mut self.data),
            id: self.id.take(),
            retry: self.retry.take(),
        })
    }
}

/// 解析 SSE 事件流。
pub fn parse_sse_events(input: &str) -> Result<Vec<SseEvent>, SseError> {
    let mut events = Vec::new();
    let mut builder = EventBuilder::default();
    let mut saw_sse_syntax = false;
    let mut event_buffer_bytes = 0usize;

    for raw_line in input.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() {
            event_buffer_bytes = 0;
            if let Some(event) = builder.finish() {
                events.push(event);
            }
            continue;
        }
        track_event_buffer_bytes(&mut event_buffer_bytes, raw_line)?;
        if line.starts_with(':') {
            saw_sse_syntax = true;
            continue;
        }

        let (field, value) = split_sse_field(line);
        if matches!(field, "event" | "data" | "id" | "retry") {
            saw_sse_syntax = true;
        }
        match field {
            "event" => builder.event = Some(value.to_string()),
            "data" => builder.push_data(value),
            "id" if !value.contains('\0') => builder.id = Some(value.to_string()),
            "retry" => {
                builder.retry = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| SseError::InvalidRetry(value.to_string()))?,
                );
            }
            _ if builder.has_data && !is_sse_metadata_line(line) => builder.push_data(line),
            _ => {}
        }
    }

    if let Some(event) = builder.finish() {
        events.push(event);
    }
    if events.is_empty() && !saw_sse_syntax && !input.trim().is_empty() {
        events.push(non_sse_response_event(input.trim()));
    }
    Ok(events)
}

/// 编码单条 SSE 事件。
pub fn encode_sse_event(event: &str, data: &str) -> String {
    encode_sse_event_with_metadata(event, data, None, None)
}

/// 编码一条完整的 SSE 事件并保留上游 `id` 与 `retry` 元数据。
pub fn encode_sse_event_with_metadata(
    event: &str,
    data: &str,
    id: Option<&str>,
    retry: Option<u64>,
) -> String {
    let mut frame = String::new();
    if let Some(id) = id {
        frame.push_str("id: ");
        frame.push_str(id);
        frame.push('\n');
    }
    if !event.is_empty() {
        frame.push_str("event: ");
        frame.push_str(event);
        frame.push('\n');
    }
    if let Some(retry) = retry {
        frame.push_str("retry: ");
        frame.push_str(&retry.to_string());
        frame.push('\n');
    }
    for line in data.split('\n') {
        frame.push_str("data: ");
        frame.push_str(line);
        frame.push('\n');
    }
    frame.push('\n');
    frame
}

fn track_event_buffer_bytes(current_bytes: &mut usize, line: &str) -> Result<(), SseError> {
    let line_separator_bytes = usize::from(*current_bytes != 0);
    *current_bytes = current_bytes
        .saturating_add(line_separator_bytes)
        .saturating_add(line.len());
    if *current_bytes > MAX_SSE_EVENT_BUFFER_BYTES {
        return Err(SseError::BufferExceeded {
            max_bytes: MAX_SSE_EVENT_BUFFER_BYTES,
        });
    }
    Ok(())
}

fn sse_frame_separator_bytes(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| (position, 2));
    let crlf = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| (position, 4));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(found), None) | (None, Some(found)) => Some(found),
        (None, None) => None,
    }
}

fn is_sse_metadata_line(line: &str) -> bool {
    line.starts_with("event:")
        || line.starts_with("data:")
        || line.starts_with("id:")
        || line.starts_with("retry:")
        || line.starts_with(':')
}

fn non_sse_response_event(raw: &str) -> SseEvent {
    let message = non_sse_error_message(raw);
    let data = serde_json::json!({
        "error": {
            "type": "error",
            "code": "non_sse_response",
            "message": message,
        }
    })
    .to_string();
    SseEvent {
        event: Some("error".to_string()),
        data,
        id: None,
        retry: None,
    }
}

fn non_sse_error_message(raw: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return raw.to_string();
    };
    value
        .get("detail")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            value
                .pointer("/error/message")
                .and_then(serde_json::Value::as_str)
        })
        .filter(|message| !message.trim().is_empty())
        .unwrap_or(raw)
        .to_string()
}

fn split_sse_field(line: &str) -> (&str, &str) {
    let Some((field, value)) = line.split_once(':') else {
        return (line, "");
    };
    (field, value.strip_prefix(' ').unwrap_or(value))
}
