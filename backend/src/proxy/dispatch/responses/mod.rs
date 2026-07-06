//! Responses 调度领域。

mod affinity;
pub(crate) mod errors;
mod event_recording;
mod live_stream;
mod prefetch;
pub(crate) mod service;
mod sse_failure;
mod stream_lifecycle;
mod trace;
