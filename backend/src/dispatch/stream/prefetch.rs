use bytes::Bytes;
use futures::StreamExt;

use crate::upstream::openai::{
    protocol::sse::{sse_frame_end, SseError},
    transport::CodexBackendSseStream,
};

use crate::dispatch::errors::ResponseDispatchError;

const MAX_STREAM_PREFETCH_BYTES: usize = 64 * 1024;

pub(in crate::dispatch) async fn prefetch_first_sse_chunk(
    mut body: CodexBackendSseStream,
) -> Result<(Bytes, CodexBackendSseStream), ResponseDispatchError> {
    let mut prefetched = Vec::new();
    while sse_frame_end(&prefetched).is_none() {
        let Some(next) = body.next().await else {
            if prefetched.is_empty() {
                return Err(ResponseDispatchError::EmptyUpstreamResponse);
            }
            return Err(ResponseDispatchError::MissingCompleted);
        };
        let chunk = next.map_err(ResponseDispatchError::Upstream)?;
        prefetched.extend_from_slice(&chunk);
        if prefetched.len() > MAX_STREAM_PREFETCH_BYTES {
            return Err(ResponseDispatchError::InvalidSse(
                SseError::BufferExceeded {
                    max_bytes: MAX_STREAM_PREFETCH_BYTES,
                },
            ));
        }
    }

    Ok((Bytes::from(prefetched), body))
}
