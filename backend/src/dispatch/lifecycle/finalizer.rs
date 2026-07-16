//! Responses live stream 的唯一结算出口。

use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::{
    dispatch::{
        controllers::{ControllerRequestScope, ControllerSet, StreamControllerContext},
        errors::ResponseDispatchStreamError,
        failure::sse::{STREAM_DISCONNECTED_CODE, STREAM_DISCONNECTED_MESSAGE},
        lifecycle::stream::StreamSummary,
        transport::canonical::{CanonicalResponseChunk, CanonicalResponseEvent},
    },
    fleet::pool::AccountLease,
    upstream::openai::protocol::sse::{
        DONE_SSE_FRAME, encode_sse_event, response_failed_sse_data_with_id,
    },
};

const LEASE_FINALIZE_TIMEOUT: Duration = Duration::from_millis(100);

/// 消费 stream context，类型层面保证每个 live loop 只能结算一次。
pub(in crate::dispatch) struct StreamFinalizer {
    controllers: ControllerSet,
    controller_scope: ControllerRequestScope,
    controller_context: StreamControllerContext,
    request_input: Vec<serde_json::Value>,
    continued_from_previous_response: bool,
    account_lease: AccountLease,
}

impl StreamFinalizer {
    pub(in crate::dispatch) fn new(
        controllers: ControllerSet,
        controller_scope: ControllerRequestScope,
        controller_context: StreamControllerContext,
        request_input: Vec<serde_json::Value>,
        continued_from_previous_response: bool,
        account_lease: AccountLease,
    ) -> Self {
        Self {
            controllers,
            controller_scope,
            controller_context,
            request_input,
            continued_from_previous_response,
            account_lease,
        }
    }

    pub(in crate::dispatch) fn response_headers(&self) -> &[(String, String)] {
        &self.controller_context.response_metadata.client_headers
    }

    pub(in crate::dispatch) fn started_at(&self) -> Instant {
        self.controller_context.started_at
    }

    pub(in crate::dispatch) fn account_id(&self) -> &str {
        &self.controller_context.account_id
    }

    pub(in crate::dispatch) fn request_input(&self) -> &[serde_json::Value] {
        &self.request_input
    }

    pub(in crate::dispatch) fn continued_from_previous_response(&self) -> bool {
        self.continued_from_previous_response
    }

    pub(in crate::dispatch) async fn finalize(
        self,
        sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
        event_sender: &mpsc::UnboundedSender<Vec<CanonicalResponseEvent>>,
        mut summary: StreamSummary,
    ) {
        let Self {
            controllers,
            controller_scope,
            controller_context,
            account_lease,
            ..
        } = self;
        let body = String::from_utf8_lossy(&summary.body).into_owned();
        controllers
            .leave_stream(controller_scope, controller_context, &summary, &body)
            .await;
        let mut lease_completion = Box::pin(account_lease.complete());
        if timeout(LEASE_FINALIZE_TIMEOUT, lease_completion.as_mut())
            .await
            .is_err()
        {
            tracing::warn!(
                timeout_ms = LEASE_FINALIZE_TIMEOUT.as_millis(),
                "Response account lease is continuing finalization in the background"
            );
            // 超时只约束客户端收尾，不取消已经开始的 slot 释放与请求用量持久化。
            drop(tokio::spawn(lease_completion));
        }
        finish_client_stream(sender, event_sender, &mut summary).await;
    }
}

async fn finish_client_stream(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    event_sender: &mpsc::UnboundedSender<Vec<CanonicalResponseEvent>>,
    summary: &mut StreamSummary,
) {
    if !summary.terminal.should_finish_client_stream() {
        return;
    }
    if flush_terminal_chunks(sender, event_sender, &mut summary.terminal_chunks)
        .await
        .is_err()
    {
        return;
    }
    if let Some(detail) = summary.terminal.synthetic_failure_detail() {
        if append_separator_if_needed(sender, event_sender, &mut summary.body)
            .await
            .is_err()
        {
            return;
        }
        let failure = response_failed_sse_data_with_id(
            summary.last_response_id.as_deref(),
            "server_error",
            STREAM_DISCONNECTED_CODE,
            &synthetic_failure_message(detail),
        );
        let bytes = Bytes::from(encode_sse_event("response.failed", &failure.to_string()));
        if append_client_chunk(
            sender,
            event_sender,
            &mut summary.body,
            CanonicalResponseChunk::new(
                bytes,
                vec![CanonicalResponseEvent::proxy_failure(failure)],
            ),
        )
        .await
        .is_err()
        {
            return;
        }
    }

    if !body_has_done(&summary.body) {
        let _ = append_client_chunk(
            sender,
            event_sender,
            &mut summary.body,
            CanonicalResponseChunk::new(Bytes::from_static(DONE_SSE_FRAME.as_bytes()), Vec::new()),
        )
        .await;
    }
}

async fn flush_terminal_chunks(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    event_sender: &mpsc::UnboundedSender<Vec<CanonicalResponseEvent>>,
    terminal_chunks: &mut Vec<CanonicalResponseChunk>,
) -> Result<(), ()> {
    for chunk in terminal_chunks.drain(..) {
        let (bytes, events) = chunk.into_parts();
        let _ = event_sender.send(events);
        sender.send(Ok(bytes)).await.map_err(|_| ())?;
    }
    Ok(())
}

async fn append_separator_if_needed(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    event_sender: &mpsc::UnboundedSender<Vec<CanonicalResponseEvent>>,
    body: &mut Vec<u8>,
) -> Result<(), ()> {
    let separator = if body.is_empty()
        || body.ends_with(b"\n\n")
        || body.ends_with(b"\r\n\r\n")
        || body.ends_with(b"\r\r")
    {
        return Ok(());
    } else if body.ends_with(b"\n") || body.ends_with(b"\r") {
        Bytes::from_static(b"\n")
    } else {
        Bytes::from_static(b"\n\n")
    };
    append_client_chunk(
        sender,
        event_sender,
        body,
        CanonicalResponseChunk::new(separator, Vec::new()),
    )
    .await
}

async fn append_client_chunk(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    event_sender: &mpsc::UnboundedSender<Vec<CanonicalResponseEvent>>,
    body: &mut Vec<u8>,
    chunk: CanonicalResponseChunk,
) -> Result<(), ()> {
    body.extend_from_slice(chunk.bytes());
    let (bytes, events) = chunk.into_parts();
    let _ = event_sender.send(events);
    sender.send(Ok(bytes)).await.map_err(|_| ())
}

fn body_has_done(body: &[u8]) -> bool {
    String::from_utf8_lossy(body)
        .trim_end_matches(['\r', '\n'])
        .ends_with(DONE_SSE_FRAME.trim_end_matches(['\r', '\n']))
}

fn synthetic_failure_message(detail: &str) -> String {
    if detail.trim().is_empty() {
        STREAM_DISCONNECTED_MESSAGE.to_string()
    } else {
        format!("{STREAM_DISCONNECTED_MESSAGE}: {detail}")
    }
}
