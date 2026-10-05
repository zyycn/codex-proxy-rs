//! WebSocket exchange 共用接收边界

use std::time::Duration;

use tokio::time::timeout;
use tungstenite::Message;

use super::super::pump::PumpedWebSocket;
use super::CodexWebSocketExchangeError;

pub(super) async fn next_websocket_message(
    websocket: &mut PumpedWebSocket,
    receive_timeout: Duration,
) -> Result<Option<Message>, CodexWebSocketExchangeError> {
    match timeout(receive_timeout, websocket.next()).await {
        Ok(message) => message.transpose().map_err(Into::into),
        Err(_) => Err(CodexWebSocketExchangeError::ReceiveIdleTimeout {
            timeout: receive_timeout,
        }),
    }
}

pub(super) fn reused_stream_receive_error(
    error: CodexWebSocketExchangeError,
) -> CodexWebSocketExchangeError {
    match error {
        CodexWebSocketExchangeError::ClosedBeforeTerminal(_)
        | CodexWebSocketExchangeError::StreamEndedBeforeTerminal { .. }
        | CodexWebSocketExchangeError::ReceiveIdleTimeout { .. }
        | CodexWebSocketExchangeError::Transport(_) => {
            reused_connection_died_before_first_event(error)
        }
        error => error,
    }
}

pub(super) fn reused_connection_died_before_first_event(
    error: CodexWebSocketExchangeError,
) -> CodexWebSocketExchangeError {
    CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstEvent {
        message: error.to_string(),
        source: Some(Box::new(error)),
    }
}
