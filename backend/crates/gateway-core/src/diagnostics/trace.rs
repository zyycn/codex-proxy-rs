//! 首部与最近事件同时保留，流式 delta 合并，所有淘汰均有计数。

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Serialize;
use serde_json::{Value, json};

use super::capture::{bounded, diagnostic_event_json, diagnostic_event_type};
use super::{body_fingerprint, diagnostic_headers};

const MAX_EVENTS: usize = 128;
const HEAD_EVENTS: usize = 8;
const MAX_DATA_BYTES: usize = 4096;
const MAX_BUFFER_BYTES: usize = 64 * 1024;

/// 显式传给异步任务的关联上下文；默认值禁用捕获，避免全局请求映射。
#[derive(Clone, Default)]
pub struct TraceContext {
    state: Option<Arc<Mutex<TraceState>>>,
    attempt_index: u32,
    exchange_id: Option<u64>,
}

impl std::fmt::Debug for TraceContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraceContext")
            .field("attempt_index", &self.attempt_index)
            .field("exchange_id", &self.exchange_id)
            .finish_non_exhaustive()
    }
}

struct TraceState {
    request_id: String,
    started_at_ms: u128,
    started: Instant,
    sequence: u64,
    exchanges: u64,
    wire_frames: u64,
    wire_bytes: u64,
    dropped_events: u64,
    bytes: usize,
    events: VecDeque<TraceEvent>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TraceEvent {
    sequence: u64,
    last_sequence: u64,
    elapsed_ms: u64,
    last_elapsed_ms: u64,
    attempt_index: u32,
    exchange_id: Option<u64>,
    stage: &'static str,
    count: u64,
    data: Value,
    #[serde(skip)]
    bytes: usize,
}

impl TraceContext {
    /// 是否已启用请求诊断，供调用方避免构造不会保存的事实。
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.state.is_some()
    }

    /// 一个模型请求只创建一次，所有 attempt 共享同一有界时间线。
    #[must_use]
    pub fn new(request_id: &str) -> Self {
        Self {
            state: Some(Arc::new(Mutex::new(TraceState {
                request_id: bounded(request_id, 128),
                started_at_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
                started: Instant::now(),
                sequence: 0,
                exchanges: 0,
                wire_frames: 0,
                wire_bytes: 0,
                dropped_events: 0,
                bytes: 0,
                events: VecDeque::new(),
            }))),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn attempt(&self, attempt_index: u32) -> Self {
        Self {
            state: self.state.clone(),
            attempt_index,
            exchange_id: None,
        }
    }

    /// 一个 attempt 中的每次实际 transport exchange 也有独立编号。
    #[must_use]
    pub fn exchange(&self, transport: &'static str) -> Self {
        let mut context = self.clone();
        if let Some(state) = &self.state {
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.exchanges += 1;
            context.exchange_id = Some(state.exchanges);
        }
        context.record("upstream.exchange.started", json!({"transport": transport}));
        context
    }

    /// details 只允许调用方构造的诊断事实；不接收原始请求/响应正文。
    pub fn record(&self, stage: &'static str, data: Value) {
        self.push(stage, data, false);
    }

    /// 同一个头部边界只采集一次：默认脱敏，显式开启 dump 时另存完整字节。
    /// facts 必须为调用方构造的安全对象；仅受控诊断头可明文保留，完整头部另存 dump。
    pub fn headers<'a>(
        &self,
        stage: &'static str,
        mut facts: Value,
        headers: impl IntoIterator<Item = (&'a str, &'a [u8])>,
    ) {
        if self.state.is_none() {
            return;
        }
        let Some(facts_object) = facts.as_object_mut() else {
            return;
        };
        let headers: Vec<_> = headers.into_iter().collect();
        let summary = diagnostic_headers(
            headers
                .iter()
                .map(|(name, value)| (*name, std::str::from_utf8(value).unwrap_or("<binary>"))),
        );
        facts_object.insert("headers".to_owned(), summary);
        self.record(stage, facts);
        if tracing::enabled!(target: "request_dump", tracing::Level::INFO) {
            let values: Vec<_> = headers
                .iter()
                .map(|(name, value)| json!({"name": name, "valueBase64": STANDARD.encode(value)}))
                .collect();
            if let Ok(bytes) = serde_json::to_vec(&json!({"headers": values})) {
                self.dump(stage, &bytes);
            }
        }
    }

    /// 入站 JSON 先记录再解析业务语义，包括未知事件和 metadata。
    /// 原文仅在现有 request_dump 开关打开时写入其独立文件。
    pub fn capture(&self, stage: &'static str, bytes: &[u8]) {
        self.dump(stage, bytes);
        self.capture_event(stage, bytes);
    }

    /// StreamCapture 已转储原始 chunk 时，只提取完整事件的安全摘要。
    pub fn capture_event(&self, stage: &'static str, bytes: &[u8]) {
        self.capture_named_event(stage, bytes, None);
    }

    pub fn capture_named_event(&self, stage: &'static str, bytes: &[u8], name: Option<&str>) {
        if self.state.is_none() {
            return;
        }
        let fingerprint = body_fingerprint(bytes);
        if bytes.len() > 128 * 1024 {
            self.record("capture.gap", json!({"reason": "json_inspection_size_limit", "stage": stage, "body": fingerprint}));
            return;
        }
        let parsed = serde_json::from_slice::<Value>(bytes).ok();
        let event_type = parsed
            .as_ref()
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str)
            .or(name);
        // 后缀只用于观测合并，不授予明文权限；未知事件用摘要区分，避免合并成同一类。
        let delta = event_type.is_some_and(|kind| kind.ends_with(".delta"));
        let data = json!({
            "body": fingerprint,
            "eventType": event_type.map(diagnostic_event_type),
            "jsonValid": parsed.is_some(),
            "metadata": parsed.as_ref().filter(|_| !delta)
                .map(|value| diagnostic_event_json(value, stage)),
        });
        self.push(stage, data, delta);
    }

    /// 只记录业务解析边界；metadata 已由 transport 在解析前捕获。
    pub fn wire_event(&self, protocol: &str, event_type: Option<&str>) {
        let delta = event_type.is_some_and(|kind| kind.ends_with(".delta"));
        self.push(
            "provider.event",
            json!({
                "protocol": protocol,
                "eventType": event_type.map(diagnostic_event_type),
            }),
            delta,
        );
    }

    /// 完整报文捕获与摘要使用相同 request / attempt / exchange ID。
    pub fn dump(&self, stage: &'static str, bytes: &[u8]) {
        if !tracing::enabled!(target: "request_dump", tracing::Level::INFO) {
            return;
        }
        if let Some(state) = &self.state {
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.wire_frames += 1;
            state.wire_bytes = state.wire_bytes.saturating_add(bytes.len() as u64);
            let wire_sequence = state.wire_frames;
            let chunk_count = bytes.len().div_ceil(32 * 1024).max(1);
            // Each log entry stays bounded even for images or very large WebSocket frames.
            for chunk_index in 0..chunk_count {
                let start = chunk_index * 32 * 1024;
                let chunk = &bytes[start..bytes.len().min(start + 32 * 1024)];
                tracing::info!(target: "request_dump", request_id = %state.request_id,
                    attempt_index = self.attempt_index, exchange_id = self.exchange_id,
                    stage, wire_sequence, chunk_index, chunk_count,
                    body_bytes = bytes.len(), body_base64 = %STANDARD.encode(chunk),
                    contains_sensitive_data = true, "diagnostic wire capture");
            }
        }
    }

    /// 与请求终态原子持久化；旧记录没有此字段，不伪造历史时间线。
    #[must_use]
    pub fn snapshot(&self) -> Option<Value> {
        let state = self
            .state
            .as_ref()?
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Some(json!({
            "schemaVersion": 1, "requestId": state.request_id,
            "startedAtUnixMs": state.started_at_ms, "totalEvents": state.sequence,
            "droppedEvents": state.dropped_events, "maxEvents": MAX_EVENTS,
            "captureMode": "sanitized", "events": state.events,
            "wireDumpEnabled": tracing::enabled!(target: "request_dump", tracing::Level::INFO),
            "wireFrames": state.wire_frames, "wireBytes": state.wire_bytes,
            "maxBufferBytes": MAX_BUFFER_BYTES, "headEvents": HEAD_EVENTS,
            "snapshotBoundary": "execution_finalization",
        }))
    }

    fn push(&self, stage: &'static str, mut data: Value, coalesce: bool) {
        let Some(state) = &self.state else { return };
        let mut encoded = serde_json::to_vec(&data).unwrap_or_default();
        if encoded.len() > MAX_DATA_BYTES {
            data = json!({"truncated": true, "summary": body_fingerprint(&encoded),
                "eventType": data.get("eventType"), "body": data.get("body"),
                "jsonValid": data.get("jsonValid"),
            });
            encoded = serde_json::to_vec(&data).unwrap_or_default();
        }
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.sequence += 1;
        let sequence = state.sequence;
        let elapsed_ms = u64::try_from(state.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if coalesce
            && let Some(last) = state.events.iter_mut().rev().find(|event| {
                event.stage == stage
                    && event.attempt_index == self.attempt_index
                    && event.exchange_id == self.exchange_id
            })
            && last.stage == stage
            && last.attempt_index == self.attempt_index
            && last.exchange_id == self.exchange_id
            && last.data.get("eventType") == data.get("eventType")
        {
            last.count += 1;
            last.last_sequence = sequence;
            last.last_elapsed_ms = elapsed_ms;
            return;
        }
        // 普通日志只输出有界安全事实；进程意外退出时仍可按 ID 检索已发生阶段。
        tracing::info!(target: "request_trace", request_id = %state.request_id,
            attempt_index = self.attempt_index, exchange_id = self.exchange_id,
            sequence, elapsed_ms, stage, data = %data, "request trace");
        let bytes = encoded.len() + 256;
        state.bytes += bytes;
        state.events.push_back(TraceEvent {
            sequence,
            last_sequence: sequence,
            elapsed_ms,
            last_elapsed_ms: elapsed_ms,
            attempt_index: self.attempt_index,
            exchange_id: self.exchange_id,
            stage,
            count: 1,
            data,
            bytes,
        });
        while state.events.len() > MAX_EVENTS || state.bytes > MAX_BUFFER_BYTES {
            // Preserve failures and retry decisions ahead of routine middle-of-stream events.
            // Head/tail and important stages compete under the same hard memory limit.
            let index = (HEAD_EVENTS..state.events.len().saturating_sub(8))
                .find(|index| {
                    !matches!(
                        state.events[*index].stage,
                        "attempt.started"
                            | "account.selection"
                            | "account.selected"
                            | "attempt.failed"
                            | "retry.decided"
                            | "upstream.close"
                            | "upstream.read.failed"
                            | "capture.gap"
                            | "request.finished"
                    )
                })
                .unwrap_or(if state.events.len() > HEAD_EVENTS {
                    HEAD_EVENTS
                } else {
                    0
                });
            if let Some(event) = state.events.remove(index) {
                state.bytes = state.bytes.saturating_sub(event.bytes);
                state.dropped_events += event.count;
            }
        }
    }
}

impl Drop for TraceState {
    fn drop(&mut self) {
        tracing::info!(target: "request_trace", request_id = %self.request_id,
            stage = "request.trace.closed", total_events = self.sequence,
            dropped_events = self.dropped_events, wire_frames = self.wire_frames,
            wire_bytes = self.wire_bytes, "request diagnostic capture closed");
    }
}
