//! WebSocket aggregate/stream 共用事件归约器。

use crate::upstream::openai::protocol::{
    events,
    websocket::{
        websocket_event_to_sse_frame, websocket_event_type, websocket_metadata_headers,
        websocket_metadata_turn_state, websocket_response_completed_id,
    },
};
use crate::upstream::openai::transport::response_meta;

use super::super::pool::{CodexWebSocketConnectionMetadata, WebSocketContinuationState};
use super::CodexWebSocketExchangeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::upstream::openai::transport::websocket) enum WebSocketTerminalKind {
    Completed,
    Incomplete,
    Failed,
}

pub(super) enum ExchangeAction {
    RateLimits(Vec<(String, String)>),
    TurnState(String),
    Forward {
        frame: String,
        terminal: Option<WebSocketTerminalKind>,
    },
    Ignore,
}

pub(super) fn reduce_websocket_event(
    raw: &str,
    metadata: &mut CodexWebSocketConnectionMetadata,
    continuation: &mut WebSocketContinuationState,
) -> Result<ExchangeAction, CodexWebSocketExchangeError> {
    if let Some(headers) = websocket_rate_limit_event_headers(raw) {
        metadata.rate_limit_headers.extend(headers.iter().cloned());
        return Ok(ExchangeAction::RateLimits(headers));
    }

    response_meta::merge_response_metadata(
        &mut metadata.response_metadata,
        websocket_metadata_headers(raw),
    );
    if let Some(turn_state) = websocket_metadata_turn_state(raw) {
        metadata.turn_state = Some(turn_state.clone());
        return Ok(ExchangeAction::TurnState(turn_state));
    }

    let event = websocket_event_type(raw);
    if event.as_deref() == Some("response.completed") {
        let response_id = websocket_response_completed_id(raw)
            .map_err(|message| CodexWebSocketExchangeError::InvalidCompletedResponse { message })?
            .ok_or_else(|| CodexWebSocketExchangeError::InvalidCompletedResponse {
                message: "response.completed is missing response id".to_string(),
            })?;
        continuation.record_completed(response_id);
    }

    let terminal = match event.as_deref() {
        Some("response.completed") => Some(WebSocketTerminalKind::Completed),
        Some("response.incomplete") => Some(WebSocketTerminalKind::Incomplete),
        Some("response.failed" | "error") => Some(WebSocketTerminalKind::Failed),
        _ => None,
    };
    Ok(match websocket_event_to_sse_frame(raw) {
        Some(frame) => ExchangeAction::Forward { frame, terminal },
        None => ExchangeAction::Ignore,
    })
}

fn websocket_rate_limit_event_headers(raw: &str) -> Option<Vec<(String, String)>> {
    events::parse_rate_limits_event_raw(raw)
        .map(|parsed| events::rate_limits_to_header_pairs(&parsed))
}
