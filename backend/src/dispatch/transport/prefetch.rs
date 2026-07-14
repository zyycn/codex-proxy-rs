//! 上游 SSE 提交边界前的规范事件预取。

use bytes::Bytes;
use futures::StreamExt;

use crate::{
    dispatch::transport::canonical::{CanonicalStreamBatch, CanonicalStreamDecoder},
    upstream::openai::{
        protocol::{responses::StreamCommitPolicy, sse::SseError},
        transport::{CodexBackendSseStream, CodexClientError},
    },
};

const MAX_STREAM_PREFETCH_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub(in crate::dispatch) enum StreamPrefetchError {
    #[error("upstream stream failed before the commit boundary: {0}")]
    Upstream(#[from] CodexClientError),
    #[error("upstream stream ended without any data")]
    Empty,
    #[error("upstream stream ended before output or a terminal event")]
    NoCommitBoundary,
    #[error("invalid upstream SSE response: {0}")]
    InvalidSse(#[from] SseError),
}

pub(in crate::dispatch) struct PrefetchedStream {
    pub bytes: Bytes,
    pub initial_batch: CanonicalStreamBatch,
    pub body: CodexBackendSseStream,
}

pub(in crate::dispatch) async fn prefetch_until_commit(
    mut body: CodexBackendSseStream,
    decoder: &mut CanonicalStreamDecoder,
    policy: StreamCommitPolicy,
) -> Result<PrefetchedStream, StreamPrefetchError> {
    let mut prefetched = Vec::new();
    let mut initial_batch = CanonicalStreamBatch { chunks: Vec::new() };
    while !decoder.commit_boundary_reached(policy) {
        let Some(next) = body.next().await else {
            if prefetched.is_empty() {
                return Err(StreamPrefetchError::Empty);
            }
            initial_batch.append(decoder.finish()?);
            if decoder.commit_boundary_reached(policy) {
                break;
            }
            return Err(StreamPrefetchError::NoCommitBoundary);
        };
        let chunk = next?;
        prefetched.extend_from_slice(&chunk);
        let batch = decoder.push(chunk)?;
        initial_batch.append(batch);
        if decoder.transport_done_seen() && !decoder.commit_boundary_reached(policy) {
            return Err(StreamPrefetchError::NoCommitBoundary);
        }
        if !decoder.commit_boundary_reached(policy) && prefetched.len() > MAX_STREAM_PREFETCH_BYTES
        {
            return Err(StreamPrefetchError::InvalidSse(SseError::BufferExceeded {
                max_bytes: MAX_STREAM_PREFETCH_BYTES,
            }));
        }
    }

    Ok(PrefetchedStream {
        bytes: Bytes::from(prefetched),
        initial_batch,
        body,
    })
}
