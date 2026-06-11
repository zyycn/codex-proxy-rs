use thiserror::Error;

use crate::codex::types::CodexResponsesRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexTransport {
    HttpSse,
    WebSocketRequired,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WebSocketSupportError {
    #[error("previous_response_id requires Codex WebSocket transport")]
    PreviousResponseRequiresWebSocket,
    #[error("request explicitly requires Codex WebSocket transport")]
    ExplicitWebSocketRequired,
}

pub fn transport_for_request(request: &CodexResponsesRequest) -> CodexTransport {
    if request.previous_response_id.is_some() || request.use_websocket {
        CodexTransport::WebSocketRequired
    } else {
        CodexTransport::HttpSse
    }
}

pub fn ensure_http_sse_supported(
    request: &CodexResponsesRequest,
) -> Result<(), WebSocketSupportError> {
    if request.previous_response_id.is_some() {
        return Err(WebSocketSupportError::PreviousResponseRequiresWebSocket);
    }
    if request.use_websocket {
        return Err(WebSocketSupportError::ExplicitWebSocketRequired);
    }
    Ok(())
}
