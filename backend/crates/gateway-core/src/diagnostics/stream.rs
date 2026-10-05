//! 仅用于观测的有界流分帧；超过限额时明确记缺口，不影响实际协议解析

use serde_json::json;

use super::TraceContext;

const MAX_FRAME_BYTES: usize = 128 * 1024;

#[derive(Clone, Copy)]
pub enum StreamFormat {
    Sse,
    JsonLines,
}

/// 同一 exchange 持有一个 observer，跨 HTTP chunk 拼接完整事件
pub struct StreamCapture {
    trace: TraceContext,
    format: StreamFormat,
    line: Vec<u8>,
    data: Vec<u8>,
    event_name: Option<String>,
    previous_cr: bool,
    first_line: bool,
    skipping_line: bool,
    skipping_event: bool,
    finished: bool,
}

impl StreamCapture {
    #[must_use]
    pub fn new(trace: TraceContext, format: StreamFormat) -> Self {
        Self {
            trace,
            format,
            line: Vec::new(),
            data: Vec::new(),
            event_name: None,
            previous_cr: false,
            first_line: true,
            skipping_line: false,
            skipping_event: false,
            finished: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.trace.dump("upstream.chunk", bytes);
        for byte in bytes {
            if self.previous_cr && *byte == b'\n' {
                self.previous_cr = false;
                continue;
            }
            self.previous_cr = *byte == b'\r';
            if matches!(*byte, b'\n' | b'\r') {
                if !self.skipping_line {
                    self.line();
                }
                self.line.clear();
                self.skipping_line = false;
            } else if !self.skipping_line {
                if self.line.len() < MAX_FRAME_BYTES {
                    self.line.push(*byte);
                } else {
                    self.trace.record(
                        "capture.gap",
                        json!({"reason": "line_size_limit", "limitBytes": MAX_FRAME_BYTES}),
                    );
                    self.line.clear();
                    self.skipping_line = true;
                    self.skipping_event = matches!(self.format, StreamFormat::Sse);
                    self.data.clear();
                }
            }
        }
    }

    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        let partial_frame = !self.line.is_empty()
            || !self.data.is_empty()
            || self.skipping_event
            || self.skipping_line;
        if !self.line.is_empty() && !self.skipping_line {
            self.line();
        }
        if !self.data.is_empty() && !self.skipping_event {
            self.trace.capture_named_event(
                "upstream.event",
                &self.data,
                self.event_name.as_deref(),
            );
        }
        self.trace
            .record("upstream.eof", json!({"partialFrame": partial_frame}));
        self.finished = true;
    }

    fn line(&mut self) {
        let line = if self.first_line {
            self.first_line = false;
            self.line
                .strip_prefix(b"\xef\xbb\xbf")
                .unwrap_or(&self.line)
        } else {
            &self.line
        };
        if matches!(self.format, StreamFormat::JsonLines) {
            if !line.is_empty() {
                self.trace.capture_event("upstream.event", line);
            }
            return;
        }
        if line.is_empty() {
            if !self.data.is_empty() && !self.skipping_event {
                self.trace.capture_named_event(
                    "upstream.event",
                    &self.data,
                    self.event_name.as_deref(),
                );
            }
            self.data.clear();
            self.event_name = None;
            self.skipping_event = false;
        } else if let Some(name) = line.strip_prefix(b"event:") {
            self.event_name = Some(super::capture::bounded(
                String::from_utf8_lossy(name).trim(),
                128,
            ));
        } else if let Some(data) = line.strip_prefix(b"data:") {
            let data = data.strip_prefix(b" ").unwrap_or(data);
            if self.skipping_event {
                return;
            }
            if self.data.len() + data.len() + 1 > MAX_FRAME_BYTES {
                self.trace.record(
                    "capture.gap",
                    json!({"reason": "event_size_limit", "limitBytes": MAX_FRAME_BYTES}),
                );
                self.data.clear();
                self.skipping_event = true;
            } else {
                if !self.data.is_empty() {
                    self.data.push(b'\n');
                }
                self.data.extend_from_slice(data);
            }
        }
    }
}

impl Drop for StreamCapture {
    fn drop(&mut self) {
        if !self.finished {
            self.trace.record(
                "upstream.reader.dropped",
                json!({
                    "partialBytes": self.line.len() + self.data.len(),
                    "captureIncomplete": self.skipping_event || self.skipping_line,
                }),
            );
        }
    }
}
