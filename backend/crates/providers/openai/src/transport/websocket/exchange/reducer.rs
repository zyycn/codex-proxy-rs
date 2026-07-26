//! WebSocket aggregate/stream 共用事件归约器。

use gateway_protocol::openai::events;
use serde_json::Value;

use crate::transport::protocol::websocket::{
    websocket_event_frame, websocket_event_type, websocket_metadata_headers,
    websocket_metadata_turn_state, websocket_response_completed_id,
};
use crate::transport::response_meta;

use super::super::pool::{CodexWebSocketConnectionMetadata, WebSocketContinuationState};
use super::CodexWebSocketExchangeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::transport::websocket) enum WebSocketTerminalKind {
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
    // 每帧只解析一次 JSON，后续提取全部复用同一 Value；
    // 不可解析的帧不承载可路由的事件类型，忽略。
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return Ok(ExchangeAction::Ignore);
    };
    if let Some(parsed) = events::parse_rate_limits_event(&value) {
        let headers = events::rate_limits_to_header_pairs(&parsed);
        metadata.rate_limit_headers.extend(headers.iter().cloned());
        return Ok(ExchangeAction::RateLimits(headers));
    }

    response_meta::merge_response_metadata(
        &mut metadata.response_metadata,
        websocket_metadata_headers(&value),
    );
    if let Some(turn_state) = websocket_metadata_turn_state(&value) {
        metadata.turn_state = Some(turn_state.clone());
        return Ok(ExchangeAction::TurnState(turn_state));
    }

    let event = websocket_event_type(&value);
    if event == Some("response.completed")
        && let Some(response_id) = websocket_response_completed_id(&value)
    {
        continuation.record_completed(response_id);
    }

    let terminal = match event {
        Some("response.completed") => Some(WebSocketTerminalKind::Completed),
        Some("response.incomplete") => Some(WebSocketTerminalKind::Incomplete),
        Some("response.failed" | "error") => Some(WebSocketTerminalKind::Failed),
        _ => None,
    };
    Ok(match websocket_event_frame(&value, raw) {
        Some(frame) => ExchangeAction::Forward { frame, terminal },
        None => ExchangeAction::Ignore,
    })
}
