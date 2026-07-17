//! Responses WebSocket endpoint、opening handshake 与首帧发送。

use std::time::Duration;

use tokio::time::timeout;
use tokio_tungstenite::{Connector, connect_async_tls_with_config};
use tungstenite::{
    self, Message,
    extensions::{ExtensionsConfig, compression::deflate::DeflateConfig},
    handshake::client::Request as WsRequest,
    http::Response as WsResponse,
    protocol::WebSocketConfig,
};

use crate::upstream::openai::{
    protocol::{
        events, responses::CodexResponsesRequest, websocket::websocket_response_create_payload_text,
    },
    transport::{endpoints::CODEX_RESPONSES_PATH, response_meta, tls},
};

use super::{
    error::CodexWebSocketExchangeError,
    model::{CodexWebSocketConnection, CodexWebSocketRequest, WebSocketContinuationRequirement},
    pool::CodexWebSocketConnectionMetadata,
    pump::{PumpKeepalive, PumpedWebSocket, RawWsStream},
};

const WEBSOCKET_EXTENSIONS: &str = "permessage-deflate; client_max_window_bits";
const WEBSOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const WEBSOCKET_SEND_TIMEOUT: Duration = Duration::from_secs(5 * 60);

impl CodexWebSocketConnection {
    /// 构造 Responses WebSocket 连接描述。
    pub fn responses(
        base_url: &str,
        websocket_key: &str,
        business_headers: Vec<(String, String)>,
    ) -> Self {
        let endpoint = responses_websocket_endpoint(base_url);
        let mut headers = Vec::new();
        if let Some(host) = websocket_host_header(&endpoint) {
            headers.push(("Host".to_string(), host));
        }
        headers.extend([
            ("Connection".to_string(), "Upgrade".to_string()),
            ("Upgrade".to_string(), "websocket".to_string()),
            ("Sec-WebSocket-Version".to_string(), "13".to_string()),
            ("Sec-WebSocket-Key".to_string(), websocket_key.to_string()),
        ]);
        headers.extend(business_headers);
        headers.push((
            "sec-websocket-extensions".to_string(),
            WEBSOCKET_EXTENSIONS.to_string(),
        ));
        Self { endpoint, headers }
    }

    /// 构造 Responses WebSocket opening 与首个 `response.create` 文本帧。
    pub fn responses_create_request(
        base_url: &str,
        websocket_key: &str,
        business_headers: Vec<(String, String)>,
        request: &CodexResponsesRequest,
    ) -> Result<CodexWebSocketRequest, serde_json::Error> {
        Ok(CodexWebSocketRequest {
            connection: Self::responses(base_url, websocket_key, business_headers),
            payload_text: websocket_response_create_payload_text(request)?,
            continuation: WebSocketContinuationRequirement::from_request(request),
        })
    }
}

/// 将 Codex backend base URL 转换为 Responses WebSocket endpoint。
pub fn responses_websocket_endpoint(base_url: &str) -> String {
    let endpoint = format!("{}{}", base_url.trim_end_matches('/'), CODEX_RESPONSES_PATH);
    if let Some(rest) = endpoint.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = endpoint.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        endpoint
    }
}

pub(super) async fn connect_pumped_websocket(
    connection: &CodexWebSocketConnection,
    keepalive: PumpKeepalive,
) -> Result<(PumpedWebSocket, WsResponse<Option<Vec<u8>>>), CodexWebSocketExchangeError> {
    let (raw, response) = connect_websocket(connection).await?;
    Ok((PumpedWebSocket::new(raw, keepalive), response))
}

pub(super) async fn send_websocket_request(
    websocket: &PumpedWebSocket,
    payload_text: &str,
) -> Result<(), CodexWebSocketExchangeError> {
    timeout(
        WEBSOCKET_SEND_TIMEOUT,
        websocket.send(Message::Text(payload_text.to_string().into())),
    )
    .await
    .map_err(|_| CodexWebSocketExchangeError::SendTimeout {
        timeout: WEBSOCKET_SEND_TIMEOUT,
    })??;
    Ok(())
}

pub(super) fn websocket_connection_metadata(
    response: &WsResponse<Option<Vec<u8>>>,
) -> CodexWebSocketConnectionMetadata {
    CodexWebSocketConnectionMetadata {
        turn_state: response_meta::turn_state(response.headers()),
        set_cookie_headers: response_meta::set_cookie_headers(response.headers()),
        rate_limit_headers: response_meta::rate_limit_headers(response.headers()),
        response_metadata: response_meta::response_metadata(response.headers()),
        diagnostics: response_meta::diagnostics(
            Some(response.status().as_u16()),
            response.headers(),
        ),
    }
}

async fn connect_websocket(
    connection: &CodexWebSocketConnection,
) -> Result<(RawWsStream, WsResponse<Option<Vec<u8>>>), CodexWebSocketExchangeError> {
    let request = websocket_handshake_request(connection)?;
    let connector = tls::maybe_build_rustls_client_config_with_custom_ca()
        .map_err(|error| {
            CodexWebSocketExchangeError::Connect(tungstenite::Error::Io(std::io::Error::other(
                error,
            )))
        })?
        .map(Connector::Rustls);
    let result = timeout(
        WEBSOCKET_CONNECT_TIMEOUT,
        connect_async_tls_with_config(request, Some(websocket_config()), false, connector),
    )
    .await
    .map_err(|_| CodexWebSocketExchangeError::ConnectTimeout {
        timeout: WEBSOCKET_CONNECT_TIMEOUT,
    })?;
    match result {
        Ok((websocket, response)) => Ok((websocket, response)),
        Err(tungstenite::Error::Http(response)) => Err(websocket_opening_error(response.as_ref())),
        Err(error) => Err(CodexWebSocketExchangeError::Connect(error)),
    }
}

fn websocket_handshake_request(
    connection: &CodexWebSocketConnection,
) -> Result<WsRequest, tungstenite::http::Error> {
    let mut builder = WsRequest::builder()
        .method("GET")
        .uri(connection.endpoint());
    for (name, value) in connection.headers() {
        if name.eq_ignore_ascii_case("sec-websocket-extensions") {
            continue;
        }
        builder = builder.header(name.as_str(), value.as_str());
    }
    builder.body(())
}

fn websocket_config() -> WebSocketConfig {
    let mut extensions = ExtensionsConfig::default();
    extensions.permessage_deflate = Some(DeflateConfig::default());

    let mut config = WebSocketConfig::default();
    config.extensions = extensions;
    config
}

fn websocket_opening_error(response: &WsResponse<Option<Vec<u8>>>) -> CodexWebSocketExchangeError {
    let status_code = response.status().as_u16();
    let body = response
        .body()
        .as_ref()
        .map(|body| String::from_utf8_lossy(body).into_owned())
        .unwrap_or_default();
    let retry_after_seconds = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .or_else(|| events::retry_after_seconds_from_body(&body));
    CodexWebSocketExchangeError::upstream(
        status_code,
        retry_after_seconds,
        body,
        response_meta::set_cookie_headers(response.headers()),
        response_meta::diagnostics(Some(status_code), response.headers()),
    )
}

fn websocket_host_header(endpoint: &str) -> Option<String> {
    let url = reqwest::Url::parse(endpoint).ok()?;
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}
