use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Instant,
};

use bytes::Bytes;
use futures::{stream::Stream, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::{
    accounts::pool::RuntimeAccountPoolService,
    dispatch::{
        affinity::{
            resolve::{evict_reasoning_replay, record_response_affinity},
            RuntimeSessionAffinityService,
        },
        errors::{backend_transport_name, ResponseDispatchStreamError},
        recording::{
            insert_first_token_ms, live_response_rate_limit_headers, live_response_turn_state,
            record_live_response_stream_event,
        },
        recovery::{cloudflare::CloudflareRecovery, reasoning_replay::ReasoningReplayCache},
        service::ResponseDispatchStream,
    },
    infra::time::elapsed_millis_i64,
    telemetry::{recorder::Recorder, usage::types::UsageRecordLevel},
    upstream::openai::{
        protocol::{
            events::extract_sse_usage,
            responses::{
                reconvert_responses_sse_event_tuple_values, response_from_codex_sse,
                response_sse_event_is_terminal, update_first_response_output_ms,
                CodexResponsesRequest, CollectedResponse,
            },
            sse::{
                encode_sse_event, parse_sse_events, response_failed_sse_event_with_id,
                sse_body_has_done, sse_frame_end, DONE_SSE_FRAME,
            },
        },
        transport::{
            CodexBackendSseStream, CodexBackendTransport, CodexRateLimitHeaderUpdates,
            CodexTurnStateUpdate, CodexUpstreamDiagnostics, WebSocketPoolDecision,
        },
    },
};

use super::{
    sse_failure::{
        sse_failure_invalid_reasoning_replay, status_code_for_stream_failure,
        stream_failure_metadata, stream_failure_source, synthetic_stream_disconnected_detail,
        STREAM_DISCONNECTED_CODE, STREAM_DISCONNECTED_MESSAGE,
    },
    trace::ResponseDispatchAttempt,
};

struct MpscResponseBodyStream {
    receiver: mpsc::Receiver<Result<Bytes, ResponseDispatchStreamError>>,
    cancel: Option<oneshot::Sender<()>>,
}

impl Drop for MpscResponseBodyStream {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

impl Stream for MpscResponseBodyStream {
    type Item = Result<Bytes, ResponseDispatchStreamError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}

pub(in crate::dispatch) fn spawn_live_response_stream(
    context: LiveResponseStreamContext,
    prefetched: Bytes,
    mut body: CodexBackendSseStream,
) -> ResponseDispatchStream {
    let (sender, receiver) = mpsc::channel(8);
    let (cancel_sender, mut cancel_receiver) = oneshot::channel();
    tokio::spawn(async move {
        let mut tuple_transformer = context
            .tuple_schema
            .clone()
            .map(TupleSseEventTransformer::new);
        let mut body_bytes = Vec::new();
        let mut first_token_ms = None;
        if !send_live_response_stream_chunk(
            &sender,
            &mut body_bytes,
            tuple_transformer.as_mut(),
            prefetched,
        )
        .await
        {
            context.account_pool.release(&context.account_id).await;
            return;
        }
        update_first_response_output_ms(context.started_at, &body_bytes, &mut first_token_ms);

        loop {
            let next = tokio::select! {
                _ = &mut cancel_receiver => {
                    context.account_pool.release(&context.account_id).await;
                    return;
                }
                next = body.next() => next,
            };
            let Some(next) = next else {
                break;
            };
            match next {
                Ok(chunk) => {
                    if !send_live_response_stream_chunk(
                        &sender,
                        &mut body_bytes,
                        tuple_transformer.as_mut(),
                        chunk,
                    )
                    .await
                    {
                        context.account_pool.release(&context.account_id).await;
                        return;
                    }
                    update_first_response_output_ms(
                        context.started_at,
                        &body_bytes,
                        &mut first_token_ms,
                    );
                }
                Err(error) => {
                    if !flush_live_response_stream_transformer(
                        &sender,
                        &mut body_bytes,
                        tuple_transformer.as_mut(),
                    )
                    .await
                    {
                        context.account_pool.release(&context.account_id).await;
                        return;
                    }
                    let detail = error.to_string();
                    let Some(body_text) =
                        send_live_response_stream_tail(&sender, &mut body_bytes, Some(&detail))
                            .await
                    else {
                        context.account_pool.release(&context.account_id).await;
                        return;
                    };
                    finalize_live_response_stream(context, body_text, first_token_ms).await;
                    return;
                }
            }
        }

        if !flush_live_response_stream_transformer(
            &sender,
            &mut body_bytes,
            tuple_transformer.as_mut(),
        )
        .await
        {
            context.account_pool.release(&context.account_id).await;
            return;
        }
        let Some(body_text) = send_live_response_stream_tail(&sender, &mut body_bytes, None).await
        else {
            context.account_pool.release(&context.account_id).await;
            return;
        };

        finalize_live_response_stream(context, body_text, first_token_ms).await;
    });

    ResponseDispatchStream {
        body: Box::pin(MpscResponseBodyStream {
            receiver,
            cancel: Some(cancel_sender),
        }),
    }
}

async fn send_live_response_stream_chunk(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    body_bytes: &mut Vec<u8>,
    transformer: Option<&mut TupleSseEventTransformer>,
    chunk: Bytes,
) -> bool {
    let chunks = match transformer {
        Some(transformer) => transformer.push(&chunk),
        None => vec![chunk],
    };
    send_live_response_stream_chunks(sender, body_bytes, chunks).await
}

async fn flush_live_response_stream_transformer(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    body_bytes: &mut Vec<u8>,
    transformer: Option<&mut TupleSseEventTransformer>,
) -> bool {
    let Some(transformer) = transformer else {
        return true;
    };
    send_live_response_stream_chunks(sender, body_bytes, transformer.finish()).await
}

async fn send_live_response_stream_chunks(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    body_bytes: &mut Vec<u8>,
    chunks: Vec<Bytes>,
) -> bool {
    for chunk in chunks {
        body_bytes.extend_from_slice(&chunk);
        if sender.send(Ok(chunk)).await.is_err() {
            return false;
        }
    }
    true
}

struct TupleSseEventTransformer {
    tuple_schema: Value,
    pending: Vec<u8>,
}

impl TupleSseEventTransformer {
    fn new(tuple_schema: Value) -> Self {
        Self {
            tuple_schema,
            pending: Vec::new(),
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<Bytes> {
        self.pending.extend_from_slice(chunk);
        let mut chunks = Vec::new();
        while let Some(frame_end) = sse_frame_end(&self.pending) {
            let frame = self.pending.drain(..frame_end).collect::<Vec<_>>();
            chunks.push(self.transform_frame(&frame));
        }
        chunks
    }

    fn finish(&mut self) -> Vec<Bytes> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let frame = std::mem::take(&mut self.pending);
        vec![self.transform_frame(&frame)]
    }

    fn transform_frame(&self, frame: &[u8]) -> Bytes {
        let frame_text = String::from_utf8_lossy(frame);
        let Ok(events) = parse_sse_events(&frame_text) else {
            return Bytes::copy_from_slice(frame);
        };
        let [event] = events.as_slice() else {
            return Bytes::copy_from_slice(frame);
        };
        let Ok(data) = serde_json::from_str::<Value>(&event.data) else {
            return Bytes::copy_from_slice(frame);
        };
        let transformed = reconvert_responses_sse_event_tuple_values(
            event.event.as_deref(),
            data,
            &self.tuple_schema,
        );
        Bytes::from(encode_sse_event(
            event.event.as_deref().unwrap_or_default(),
            &transformed.to_string(),
        ))
    }
}

async fn send_live_response_stream_tail(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    body_bytes: &mut Vec<u8>,
    failure_detail: Option<&str>,
) -> Option<String> {
    let mut body_text = String::from_utf8_lossy(body_bytes).to_string();
    if !sse_body_has_terminal_event(&body_text) {
        if let Some(separator) = missing_sse_event_separator(&body_text) {
            body_text.push_str(separator);
            body_bytes.extend_from_slice(separator.as_bytes());
            if sender
                .send(Ok(Bytes::copy_from_slice(separator.as_bytes())))
                .await
                .is_err()
            {
                return None;
            }
        }
        let failure =
            premature_close_failed_event(latest_response_id(&body_text).as_deref(), failure_detail);
        body_text.push_str(&failure);
        body_bytes.extend_from_slice(failure.as_bytes());
        if sender.send(Ok(Bytes::from(failure))).await.is_err() {
            return None;
        }
    }

    if !sse_body_has_done(&body_text) {
        body_text.push_str(DONE_SSE_FRAME);
        body_bytes.extend_from_slice(DONE_SSE_FRAME.as_bytes());
        if sender
            .send(Ok(Bytes::from_static(DONE_SSE_FRAME.as_bytes())))
            .await
            .is_err()
        {
            return None;
        }
    }

    Some(body_text)
}

fn sse_body_has_terminal_event(body: &str) -> bool {
    parse_sse_events(body).is_ok_and(|events| events.iter().any(response_sse_event_is_terminal))
}

fn missing_sse_event_separator(body: &str) -> Option<&'static str> {
    if body.is_empty()
        || body.ends_with("\n\n")
        || body.ends_with("\r\n\r\n")
        || body.ends_with("\r\r")
    {
        None
    } else if body.ends_with('\n') || body.ends_with('\r') {
        Some("\n")
    } else {
        Some("\n\n")
    }
}

pub(in crate::dispatch) struct LiveResponseStreamContext {
    pub(in crate::dispatch) account_pool: Arc<RuntimeAccountPoolService>,
    pub(in crate::dispatch) session_affinity: Arc<RuntimeSessionAffinityService>,
    pub(in crate::dispatch) reasoning_replay: Arc<Mutex<ReasoningReplayCache>>,
    pub(in crate::dispatch) recorder: Arc<Recorder>,
    pub(in crate::dispatch) cloudflare: CloudflareRecovery,
    pub(in crate::dispatch) account_id: String,
    pub(in crate::dispatch) account_plan_type: Option<String>,
    pub(in crate::dispatch) request_id: String,
    pub(in crate::dispatch) route: String,
    pub(in crate::dispatch) model: String,
    pub(in crate::dispatch) display_model: String,
    pub(in crate::dispatch) requested_model: String,
    pub(in crate::dispatch) client_ip: Option<String>,
    pub(in crate::dispatch) request: CodexResponsesRequest,
    pub(in crate::dispatch) tuple_schema: Option<Value>,
    pub(in crate::dispatch) transport: CodexBackendTransport,
    pub(in crate::dispatch) rate_limit_headers: Vec<(String, String)>,
    pub(in crate::dispatch) rate_limit_header_updates: Option<CodexRateLimitHeaderUpdates>,
    pub(in crate::dispatch) turn_state_update: Option<CodexTurnStateUpdate>,
    pub(in crate::dispatch) websocket_pool_decision: Option<WebSocketPoolDecision>,
    pub(in crate::dispatch) turn_state: Option<String>,
    pub(in crate::dispatch) diagnostics: CodexUpstreamDiagnostics,
    pub(in crate::dispatch) attempt: ResponseDispatchAttempt,
    pub(in crate::dispatch) attempts: Vec<ResponseDispatchAttempt>,
    pub(in crate::dispatch) started_at: Instant,
}

pub(in crate::dispatch) fn latest_response_id(body: &str) -> Option<String> {
    parse_sse_events(body).ok().and_then(|events| {
        events.iter().rev().find_map(|event| {
            serde_json::from_str::<Value>(&event.data)
                .ok()
                .and_then(|data| {
                    data.pointer("/response/id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.trim().is_empty())
                        .map(ToString::to_string)
                })
        })
    })
}

pub(in crate::dispatch) fn premature_close_failed_event(
    response_id: Option<&str>,
    detail: Option<&str>,
) -> String {
    let message = match detail.filter(|value| !value.trim().is_empty()) {
        Some(detail) => format!("{STREAM_DISCONNECTED_MESSAGE}: {detail}"),
        None => STREAM_DISCONNECTED_MESSAGE.to_string(),
    };
    response_failed_sse_event_with_id(
        response_id,
        "server_error",
        STREAM_DISCONNECTED_CODE,
        &message,
    )
}

pub(in crate::dispatch) async fn finalize_live_response_stream(
    context: LiveResponseStreamContext,
    body: String,
    first_token_ms: Option<i64>,
) {
    let rate_limit_headers = live_response_rate_limit_headers(&context).await;
    context
        .account_pool
        .sync_passive_rate_limit_headers_for_account(
            &context.account_id,
            context.account_plan_type.as_deref(),
            &rate_limit_headers,
        )
        .await;
    let turn_state = live_response_turn_state(&context).await;
    let usage = match extract_sse_usage(&body) {
        Ok(Some(usage)) => {
            context
                .account_pool
                .record_token_usage(&context.account_id, &context.model, &usage)
                .await;
            Some(usage)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(account_id = %context.account_id, error = %error, "failed to extract streaming token usage");
            None
        }
    };

    match response_from_codex_sse(&body, context.tuple_schema.as_ref()) {
        Ok(CollectedResponse::Completed(completed)) => {
            context
                .cloudflare
                .reset_account_recovery(&context.account_id)
                .await;
            let response_id = completed.get("id").and_then(Value::as_str);
            record_response_affinity(
                &context.session_affinity,
                &context.reasoning_replay,
                &context.request,
                &context.account_id,
                &body,
                turn_state,
                usage,
            )
            .await;
            record_live_response_stream_event(
                &context,
                200,
                UsageRecordLevel::Info,
                "v1 responses stream completed",
                serde_json::json!({
                    "stream": true,
                    "completed": true,
                    "responseId": response_id,
                    "firstTokenMs": first_token_ms,
                    "usage": usage,
                }),
                &rate_limit_headers,
                &body,
            )
            .await;
        }
        Ok(CollectedResponse::Failed(failure)) => {
            if sse_failure_invalid_reasoning_replay(&failure) {
                evict_reasoning_replay(
                    &context.reasoning_replay,
                    &context.request,
                    &context.account_id,
                )
                .await;
            }
            let response_id = latest_response_id(&body);
            let latency_ms = elapsed_millis_i64(context.started_at);
            let failure_source = stream_failure_source(&failure);
            let failure_detail = synthetic_stream_disconnected_detail(&failure);
            let websocket_pool_kind = context
                .websocket_pool_decision
                .map(|decision| decision.kind());
            let websocket_pool_reason = context
                .websocket_pool_decision
                .and_then(|decision| decision.reason());
            tracing::warn!(
                account_id = %context.account_id,
                request_id = %context.request_id,
                response_id = response_id.as_deref().unwrap_or(""),
                transport = %backend_transport_name(context.transport),
                websocket_pool_kind = ?websocket_pool_kind,
                websocket_pool_reason = ?websocket_pool_reason,
                first_token_ms = ?first_token_ms,
                latency_ms,
                event = %failure.event,
                code = ?failure.upstream_code.as_deref(),
                failure_source = %failure_source,
                failure_detail = ?failure_detail.as_deref(),
                "live upstream stream ended with response.failed"
            );
            let mut metadata = stream_failure_metadata(&failure, usage);
            insert_first_token_ms(&mut metadata, first_token_ms);
            record_live_response_stream_event(
                &context,
                status_code_for_stream_failure(&failure),
                UsageRecordLevel::Error,
                "v1 responses stream failed",
                metadata,
                &rate_limit_headers,
                &body,
            )
            .await;
        }
        Ok(CollectedResponse::MissingCompleted | CollectedResponse::Empty) => {
            tracing::warn!(
                account_id = %context.account_id,
                "live upstream stream ended without response.completed"
            );
            let mut metadata = serde_json::json!({
                "stream": true,
                "failed": true,
                "upstreamCode": "missing_completed",
                "usage": usage,
            });
            insert_first_token_ms(&mut metadata, first_token_ms);
            record_live_response_stream_event(
                &context,
                502,
                UsageRecordLevel::Error,
                "v1 responses stream ended without response.completed",
                metadata,
                &rate_limit_headers,
                &body,
            )
            .await;
        }
        Err(error) => {
            tracing::warn!(account_id = %context.account_id, error = %error, "failed to parse completed live stream");
            let mut metadata = serde_json::json!({
                "stream": true,
                "sseParseError": error.to_string(),
                "usage": usage,
            });
            insert_first_token_ms(&mut metadata, first_token_ms);
            record_live_response_stream_event(
                &context,
                502,
                UsageRecordLevel::Warn,
                "v1 responses stream SSE response invalid",
                metadata,
                &rate_limit_headers,
                &body,
            )
            .await;
        }
    }

    context.account_pool.release(&context.account_id).await;
}
