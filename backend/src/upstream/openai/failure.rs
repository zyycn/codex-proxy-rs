//! Codex 上游原始错误到稳定失败事实的规范化。

use serde_json::Value;

use crate::upstream::openai::transport::{
    CodexClientError,
    websocket::{CodexWebSocketExchangeError, PreviousResponseUnavailableReason},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpstreamFailureKind {
    HttpStatus,
    HttpConnect,
    HttpTimeout,
    HttpTransport,
    StreamIdle,
    WebSocketUpstream,
    WebSocketTransport,
    WebSocketTimeout,
    WebSocketProtocol,
    PostSendAmbiguous,
    ContinuationUnavailable(PreviousResponseUnavailableReason),
    RequestEncoding,
    Protocol,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpstreamFailureFacts {
    pub kind: UpstreamFailureKind,
    pub status_code: Option<u16>,
    pub code: Option<String>,
    pub error_type: Option<String>,
    pub identity_authorization_error: Option<String>,
    pub identity_error_code: Option<String>,
    pub message: String,
    pub body: String,
    pub retry_after_seconds: Option<u64>,
}

pub fn upstream_failure_facts(error: &CodexClientError) -> UpstreamFailureFacts {
    match error {
        CodexClientError::Http(error) => UpstreamFailureFacts {
            kind: if error.is_connect() {
                UpstreamFailureKind::HttpConnect
            } else if error.is_timeout() {
                UpstreamFailureKind::HttpTimeout
            } else {
                UpstreamFailureKind::HttpTransport
            },
            status_code: error.status().map(|status| status.as_u16()),
            code: None,
            error_type: None,
            identity_authorization_error: None,
            identity_error_code: None,
            message: error.to_string(),
            body: error.to_string(),
            retry_after_seconds: None,
        },
        CodexClientError::Upstream {
            status,
            body,
            retry_after_seconds,
            transport,
            diagnostics,
            ..
        } => {
            let (code, error_type, message) = error_fields(body);
            UpstreamFailureFacts {
                kind: match transport {
                    crate::upstream::openai::transport::CodexBackendTransport::HttpSse => {
                        UpstreamFailureKind::HttpStatus
                    }
                    crate::upstream::openai::transport::CodexBackendTransport::WebSocket => {
                        UpstreamFailureKind::WebSocketUpstream
                    }
                },
                status_code: Some(status.as_u16()),
                code,
                error_type,
                identity_authorization_error: diagnostics.identity_authorization_error.clone(),
                identity_error_code: diagnostics.identity_error_code.clone(),
                message: message.unwrap_or_else(|| body.clone()),
                body: body.clone(),
                retry_after_seconds: *retry_after_seconds,
            }
        }
        CodexClientError::StreamIdleTimeout { .. } => {
            simple_facts(UpstreamFailureKind::StreamIdle, error)
        }
        CodexClientError::WebSocket(CodexWebSocketExchangeError::Upstream(upstream)) => {
            let (body_code, body_type, body_message) = error_fields(&upstream.body);
            UpstreamFailureFacts {
                kind: UpstreamFailureKind::WebSocketUpstream,
                status_code: Some(upstream.status_code),
                code: body_code,
                error_type: body_type,
                identity_authorization_error: upstream
                    .diagnostics
                    .identity_authorization_error
                    .clone(),
                identity_error_code: upstream.diagnostics.identity_error_code.clone(),
                message: body_message.unwrap_or_else(|| upstream.body.clone()),
                body: upstream.body.clone(),
                retry_after_seconds: upstream.retry_after_seconds,
            }
        }
        CodexClientError::WebSocket(CodexWebSocketExchangeError::ContinuationUnavailable {
            reason,
        }) => simple_facts(UpstreamFailureKind::ContinuationUnavailable(*reason), error),
        CodexClientError::WebSocket(
            CodexWebSocketExchangeError::ConnectTimeout { .. }
            | CodexWebSocketExchangeError::FastPathTimeout { .. }
            | CodexWebSocketExchangeError::SendTimeout { .. }
            | CodexWebSocketExchangeError::ReceiveIdleTimeout { .. }
            | CodexWebSocketExchangeError::InitialEventTimeout { .. },
        ) => simple_facts(UpstreamFailureKind::WebSocketTimeout, error),
        CodexClientError::WebSocket(
            CodexWebSocketExchangeError::Transport(_)
            | CodexWebSocketExchangeError::Connect(_)
            | CodexWebSocketExchangeError::OriginCircuitOpen
            | CodexWebSocketExchangeError::OriginHalfOpenBusy
            | CodexWebSocketExchangeError::SharedConnectFailed
            | CodexWebSocketExchangeError::ClosedBeforeTerminal
            | CodexWebSocketExchangeError::ReusedConnectionDiedBeforeFirstEvent { .. },
        ) => simple_facts(UpstreamFailureKind::WebSocketTransport, error),
        CodexClientError::WebSocket(CodexWebSocketExchangeError::PostSendAmbiguous { .. }) => {
            simple_facts(UpstreamFailureKind::PostSendAmbiguous, error)
        }
        CodexClientError::WebSocket(
            CodexWebSocketExchangeError::InvalidSse(_)
            | CodexWebSocketExchangeError::InvalidCompletedResponse { .. }
            | CodexWebSocketExchangeError::UnexpectedBinaryEvent,
        ) => simple_facts(UpstreamFailureKind::WebSocketProtocol, error),
        CodexClientError::WebSocket(CodexWebSocketExchangeError::InvalidRequest(_))
        | CodexClientError::WebSocketEncode(_)
        | CodexClientError::InvalidHeaderName(_)
        | CodexClientError::InvalidHeaderValue(_)
        | CodexClientError::CustomCa(_) => {
            simple_facts(UpstreamFailureKind::RequestEncoding, error)
        }
        CodexClientError::InvalidSse(_) => simple_facts(UpstreamFailureKind::Protocol, error),
    }
}

fn simple_facts(kind: UpstreamFailureKind, error: &CodexClientError) -> UpstreamFailureFacts {
    let message = error.to_string();
    UpstreamFailureFacts {
        kind,
        status_code: None,
        code: None,
        error_type: None,
        identity_authorization_error: None,
        identity_error_code: None,
        body: message.clone(),
        message,
        retry_after_seconds: None,
    }
}

fn error_fields(body: &str) -> (Option<String>, Option<String>, Option<String>) {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return (None, None, None);
    };
    let error = value
        .pointer("/response/error")
        .or_else(|| value.get("error"))
        .or_else(|| value.get("detail"))
        .unwrap_or(&value);
    (
        error
            .get("code")
            .or_else(|| value.get("code"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        error
            .get("type")
            .or_else(|| value.get("type"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        error
            .get("message")
            .or_else(|| value.get("message"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
    )
}
