use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};

use bytes::Bytes;
use futures::{StreamExt, stream::Stream};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{
    dispatch::{
        errors::ResponseDispatchStreamError,
        lifecycle::{
            finalizer::StreamFinalizer,
            stream::{StreamSummary, StreamTerminal},
        },
        service::ResponseDispatchStream,
        transport::canonical::{
            CanonicalResponseChunk, CanonicalResponseEvent, CanonicalStreamBatch,
            CanonicalStreamDecoder,
        },
    },
    infra::time::elapsed_millis_i64,
    upstream::openai::transport::CodexBackendSseStream,
};

const MAX_LIVE_RESPONSE_CAPTURE_BYTES: usize = 16 * 1024 * 1024;

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
    finalizer: StreamFinalizer,
    decoder: CanonicalStreamDecoder,
    initial_batch: CanonicalStreamBatch,
    first_event_ms: i64,
    body: CodexBackendSseStream,
    shutdown: CancellationToken,
) -> ResponseDispatchStream {
    let account_id = finalizer.account_id().to_string();
    let request_input = finalizer.request_input().to_vec();
    let continued_from_previous_response = finalizer.continued_from_previous_response();
    let response_headers = finalizer.response_headers().to_vec();
    let started_at = finalizer.started_at();
    let (sender, receiver) = mpsc::channel(8);
    let (event_sender, canonical_events) = mpsc::unbounded_channel();
    let (cancel_sender, cancel_receiver) = oneshot::channel();
    tokio::spawn(async move {
        let summary = run_live_response_stream(
            &sender,
            &event_sender,
            LiveRunInputs {
                initial_batch,
                first_event_ms,
                body,
                cancel: cancel_receiver,
                decoder,
                started_at,
                shutdown,
            },
        )
        .await;
        finalizer.finalize(&sender, &event_sender, summary).await;
    });

    ResponseDispatchStream {
        account_id,
        request_input,
        continued_from_previous_response,
        body: Box::pin(MpscResponseBodyStream {
            receiver,
            cancel: Some(cancel_sender),
        }),
        canonical_events,
        response_headers,
    }
}

struct LiveRunInputs {
    initial_batch: CanonicalStreamBatch,
    first_event_ms: i64,
    body: CodexBackendSseStream,
    cancel: oneshot::Receiver<()>,
    decoder: CanonicalStreamDecoder,
    started_at: Instant,
    shutdown: CancellationToken,
}

async fn run_live_response_stream(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    event_sender: &mpsc::UnboundedSender<Vec<CanonicalResponseEvent>>,
    inputs: LiveRunInputs,
) -> StreamSummary {
    let LiveRunInputs {
        initial_batch,
        first_event_ms,
        mut body,
        mut cancel,
        mut decoder,
        started_at,
        shutdown,
    } = inputs;
    let mut body_bytes = Vec::new();
    let mut terminal_chunks = Vec::new();
    let mut first_token_ms = None;
    let mut first_reasoning_ms = None;
    let mut first_text_ms = None;

    let decoded_terminal = take_stream_terminal(&mut decoder);
    let terminal = if decoded_terminal.is_some() {
        stage_terminal_batch(&mut body_bytes, &mut terminal_chunks, initial_batch)
            .err()
            .or(decoded_terminal)
    } else {
        forward_batch(sender, event_sender, &mut body_bytes, initial_batch)
            .await
            .err()
    };
    update_output_timing_ms(
        &decoder,
        started_at,
        &mut first_token_ms,
        &mut first_reasoning_ms,
        &mut first_text_ms,
    );
    if let Some(terminal) = terminal {
        return stream_summary(
            terminal,
            body_bytes,
            terminal_chunks,
            (first_token_ms, first_reasoning_ms, first_text_ms),
            first_event_ms,
            &decoder,
        );
    }

    let mut shutdown = Box::pin(shutdown.cancelled_owned());
    let terminal = loop {
        let next = tokio::select! {
            _ = &mut cancel => break StreamTerminal::Cancelled,
            () = &mut shutdown => break StreamTerminal::Shutdown,
            next = body.next() => next,
        };
        match next {
            Some(Ok(chunk)) => match decoder.push(chunk) {
                Ok(batch) => {
                    update_output_timing_ms(
                        &decoder,
                        started_at,
                        &mut first_token_ms,
                        &mut first_reasoning_ms,
                        &mut first_text_ms,
                    );
                    if let Some(terminal) = take_stream_terminal(&mut decoder) {
                        match stage_terminal_batch(&mut body_bytes, &mut terminal_chunks, batch) {
                            Ok(()) => break terminal,
                            Err(terminal) => break terminal,
                        }
                    }
                    if let Err(terminal) =
                        forward_batch(sender, event_sender, &mut body_bytes, batch).await
                    {
                        break terminal;
                    }
                }
                Err(error) => {
                    break StreamTerminal::ProtocolError {
                        detail: error.to_string(),
                    };
                }
            },
            Some(Err(error)) => {
                break finish_decoder(
                    sender,
                    event_sender,
                    &mut body_bytes,
                    &mut terminal_chunks,
                    &mut decoder,
                    StreamTerminal::UpstreamError {
                        detail: error.to_string(),
                    },
                )
                .await;
            }
            None => {
                break finish_decoder(
                    sender,
                    event_sender,
                    &mut body_bytes,
                    &mut terminal_chunks,
                    &mut decoder,
                    StreamTerminal::UpstreamClosed,
                )
                .await;
            }
        }
    };

    update_output_timing_ms(
        &decoder,
        started_at,
        &mut first_token_ms,
        &mut first_reasoning_ms,
        &mut first_text_ms,
    );
    stream_summary(
        terminal,
        body_bytes,
        terminal_chunks,
        (first_token_ms, first_reasoning_ms, first_text_ms),
        first_event_ms,
        &decoder,
    )
}

async fn finish_decoder(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    event_sender: &mpsc::UnboundedSender<Vec<CanonicalResponseEvent>>,
    body_bytes: &mut Vec<u8>,
    terminal_chunks: &mut Vec<CanonicalResponseChunk>,
    decoder: &mut CanonicalStreamDecoder,
    fallback: StreamTerminal,
) -> StreamTerminal {
    let batch = match decoder.finish() {
        Ok(batch) => batch,
        Err(error) => {
            return StreamTerminal::ProtocolError {
                detail: error.to_string(),
            };
        }
    };
    if let Some(terminal) = take_stream_terminal(decoder) {
        return match stage_terminal_batch(body_bytes, terminal_chunks, batch) {
            Ok(()) => terminal,
            Err(terminal) => terminal,
        };
    }
    if let Err(terminal) = forward_batch(sender, event_sender, body_bytes, batch).await {
        return terminal;
    }
    fallback
}

fn take_stream_terminal(decoder: &mut CanonicalStreamDecoder) -> Option<StreamTerminal> {
    let terminal = decoder.take_terminal().map(StreamTerminal::from);
    let transport_done = decoder.take_transport_done();
    terminal.or_else(|| transport_done.then_some(StreamTerminal::UpstreamClosed))
}

fn stage_terminal_batch(
    body_bytes: &mut Vec<u8>,
    terminal_chunks: &mut Vec<CanonicalResponseChunk>,
    batch: CanonicalStreamBatch,
) -> Result<(), StreamTerminal> {
    let batch_bytes = batch.chunks.iter().fold(0usize, |total, chunk| {
        total.saturating_add(chunk.bytes().len())
    });
    ensure_capture_capacity(body_bytes.len(), batch_bytes)?;
    for chunk in batch.chunks {
        body_bytes.extend_from_slice(chunk.bytes());
        terminal_chunks.push(chunk);
    }
    Ok(())
}

async fn forward_batch(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    event_sender: &mpsc::UnboundedSender<Vec<CanonicalResponseEvent>>,
    body_bytes: &mut Vec<u8>,
    batch: CanonicalStreamBatch,
) -> Result<(), StreamTerminal> {
    for chunk in batch.chunks {
        capture_chunk(body_bytes, &chunk)?;
        let (bytes, events) = chunk.into_parts();
        let _ = event_sender.send(events);
        if sender.send(Ok(bytes)).await.is_err() {
            return Err(StreamTerminal::DownstreamClosed);
        }
    }
    Ok(())
}

fn capture_chunk(
    body_bytes: &mut Vec<u8>,
    chunk: &CanonicalResponseChunk,
) -> Result<(), StreamTerminal> {
    ensure_capture_capacity(body_bytes.len(), chunk.bytes().len())?;
    body_bytes.extend_from_slice(chunk.bytes());
    Ok(())
}

fn ensure_capture_capacity(
    captured_bytes: usize,
    incoming_bytes: usize,
) -> Result<(), StreamTerminal> {
    if captured_bytes.saturating_add(incoming_bytes) > MAX_LIVE_RESPONSE_CAPTURE_BYTES {
        tracing::warn!(
            captured_bytes,
            incoming_bytes,
            capture_limit_bytes = MAX_LIVE_RESPONSE_CAPTURE_BYTES,
            "Live response capture limit exceeded"
        );
        return Err(StreamTerminal::CaptureLimitExceeded);
    }
    Ok(())
}

fn update_output_timing_ms(
    decoder: &CanonicalStreamDecoder,
    started_at: Instant,
    first_token_ms: &mut Option<i64>,
    first_reasoning_ms: &mut Option<i64>,
    first_text_ms: &mut Option<i64>,
) {
    let first_token_missing = first_token_ms.is_none() && decoder.first_semantic_output_seen();
    let first_reasoning_missing =
        first_reasoning_ms.is_none() && decoder.first_reasoning_output_seen();
    let first_text_missing = first_text_ms.is_none() && decoder.first_text_output_seen();
    if !(first_token_missing || first_reasoning_missing || first_text_missing) {
        return;
    }
    let elapsed_ms = elapsed_millis_i64(started_at).max(1);
    if first_token_missing {
        *first_token_ms = Some(elapsed_ms);
    }
    if first_reasoning_missing {
        *first_reasoning_ms = Some(elapsed_ms);
    }
    if first_text_missing {
        *first_text_ms = Some(elapsed_ms);
    }
}

fn stream_summary(
    terminal: StreamTerminal,
    body: Vec<u8>,
    terminal_chunks: Vec<CanonicalResponseChunk>,
    output_timing: (Option<i64>, Option<i64>, Option<i64>),
    first_event_ms: i64,
    decoder: &CanonicalStreamDecoder,
) -> StreamSummary {
    let (first_token_ms, first_reasoning_ms, first_text_ms) = output_timing;
    StreamSummary {
        terminal,
        body,
        terminal_chunks,
        first_token_ms,
        first_reasoning_ms,
        first_text_ms,
        first_event_ms,
        usage: decoder.usage(),
        last_response_id: decoder.last_response_id().map(ToString::to_string),
    }
}
