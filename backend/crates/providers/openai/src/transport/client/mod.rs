//! Codex 上游客户端的模块门面

mod backend;
mod catalog;
mod json;
mod live;
mod messages;
mod stream;

pub use backend::{
    CodexAccountSelectionTelemetry, CodexBackendClient, CodexBackendJsonResponse,
    CodexBackendSseStream, CodexBackendStreamingResponse, CodexBackendTransport, CodexClientError,
    CodexClientResult, CodexClientVisibleUpstreamResponse, CodexRateLimitUpdates,
    CodexRequestContext, CodexResponseMetadataUpdates, CodexTransportDecision,
    CodexTransportMetrics, build_account_http_client, build_reqwest_client,
};
pub(super) use backend::{
    OpenAiUpstreamProtocol, openai_subagent_from_metadata, read_capped_response_body,
    retry_after_seconds, truncate_for_error,
};
pub use live::CodexLiveCallResponse;
