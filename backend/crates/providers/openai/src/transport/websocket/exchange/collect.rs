//! 聚合式 WebSocket exchange。

use std::time::{Duration, Instant};

use gateway_protocol::openai::{events, sse::SseEventDecoder};
use tungstenite::Message;

use crate::transport::response_meta;

use super::super::{
    pool::{CodexWebSocketConnectionMetadata, WebSocketContinuationState},
    pump::PumpedWebSocket,
};
use super::io::{
    next_websocket_message, receive_idle_timeout, reused_connection_died_before_first_event,
};
use super::reducer::{ExchangeAction, WebSocketTerminalKind, reduce_websocket_event};
use super::{CodexWebSocketExchange, CodexWebSocketExchangeError};

pub(in crate::transport::websocket) struct CollectedWebSocket {
    pub(in crate::transport::websocket) exchange: CodexWebSocketExchange,
    pub(in crate::transport::websocket) websocket: PumpedWebSocket,
    pub(in crate::transport::websocket) metadata: CodexWebSocketConnectionMetadata,
    pub(in crate::transport::websocket) continuation: WebSocketContinuationState,
    pub(in crate::transport::websocket) terminal: WebSocketTerminalKind,
}

pub(in crate::transport::websocket) async fn collect_websocket_response(
    mut websocket: PumpedWebSocket,
    mut metadata: CodexWebSocketConnectionMetadata,
    mut continuation: WebSocketContinuationState,
    reused_connection: bool,
    started_at: Instant,
    initial_event_timeout: Option<Duration>,
) -> Result<CollectedWebSocket, CodexWebSocketExchangeError> {
    let mut body = String::new();
    let mut saw_upstream_activity = false;
    let mut first_token_ms = None;
    let mut first_reasoning_ms = None;
    let mut first_text_ms = None;
    let mut first_event_ms = None;
    let mut output_decoder = SseEventDecoder::default();

    loop {
        let receive_timeout = receive_idle_timeout(saw_upstream_activity, initial_event_timeout);
        let message = match next_websocket_message(&mut websocket, receive_timeout).await {
            Ok(message) => message,
            Err(CodexWebSocketExchangeError::ReceiveIdleTimeout { timeout })
                if !saw_upstream_activity =>
            {
                if reused_connection {
                    return Err(reused_connection_died_before_first_event(
                        &CodexWebSocketExchangeError::InitialEventTimeout { timeout },
                    ));
                }
                return Err(CodexWebSocketExchangeError::InitialEventTimeout { timeout });
            }
            Err(error) => return Err(error),
        };
        let Some(message) = message else {
            break;
        };
        let text = match message {
            Message::Text(text) => {
                saw_upstream_activity = true;
                first_event_ms.get_or_insert_with(|| {
                    crate::transport::time::elapsed_millis_i64(started_at).max(1)
                });
                text
            }
            Message::Binary(_) => return Err(CodexWebSocketExchangeError::UnexpectedBinaryEvent),
            Message::Close(_) if reused_connection && !saw_upstream_activity => {
                return Err(
                    CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstEvent {
                        message: "websocket closed".to_string(),
                    },
                );
            }
            Message::Close(_) => break,
            _ => continue,
        };
        let raw = text.to_string();
        let ExchangeAction::Forward { frame, terminal } =
            reduce_websocket_event(&raw, &mut metadata, &mut continuation)?
        else {
            continue;
        };
        body.push_str(&frame);
        response_meta::update_response_timing_ms(
            started_at,
            &mut output_decoder,
            frame.as_bytes(),
            &mut first_token_ms,
            &mut first_reasoning_ms,
            &mut first_text_ms,
        );
        if let Some(terminal) = terminal {
            let usage = events::extract_sse_usage(&body)?;
            let exchange = CodexWebSocketExchange {
                body,
                usage,
                turn_state: metadata.turn_state.clone(),
                set_cookie_headers: metadata.set_cookie_headers.clone(),
                rate_limit_headers: metadata.rate_limit_headers.clone(),
                first_token_ms,
                first_reasoning_ms,
                first_text_ms,
                first_event_ms,
                pool_decision: None,
                connection_local_continuation_expires_at: None,
                diagnostics: metadata.diagnostics.clone(),
                response_metadata: metadata.response_metadata.clone(),
            };
            return Ok(CollectedWebSocket {
                exchange,
                websocket,
                metadata,
                continuation,
                terminal,
            });
        }
    }

    if reused_connection && !saw_upstream_activity {
        return Err(
            CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstEvent {
                message: "websocket closed before terminal event".to_string(),
            },
        );
    }

    Err(CodexWebSocketExchangeError::ClosedBeforeTerminal)
}
