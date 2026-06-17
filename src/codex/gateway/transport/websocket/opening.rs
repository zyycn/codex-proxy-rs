use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rustls_pki_types::ServerName;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::TlsConnector;
use tokio_tungstenite::{
    tungstenite::{
        error::{CapacityError, Error as WsError, ProtocolError, TlsError, UrlError},
        handshake::{
            client::{generate_key, Response as WsResponse},
            derive_accept_key,
        },
        http::{Request as WsRequest, StatusCode, Uri},
        protocol::Role,
    },
    MaybeTlsStream, WebSocketStream,
};

use super::deflate::PerMessageDeflateStream;
use super::pool::CodexWsStream;
use crate::codex::gateway::transport::custom_ca::{
    maybe_build_rustls_client_config_with_custom_ca, native_root_store,
};

const MAX_OPENING_RESPONSE_BYTES: usize = 64 * 1024;
const ORIGINAL_WS_PERMESSAGE_DEFLATE_EXTENSION: &str = "permessage-deflate; client_max_window_bits";
const REDACTED_VALUE: &str = "<redacted>";

const ORIGINAL_WS_HEADER_ORDER: &[&str] = &[
    "chatgpt-account-id",
    "authorization",
    "user-agent",
    "originator",
    "openai-beta",
    "x-codex-beta-features",
    "x-client-request-id",
    "session_id",
    "thread-id",
    "x-codex-window-id",
    "x-codex-turn-metadata",
    "x-codex-turn-state",
    "x-responsesapi-include-timing-metrics",
    "version",
    "x-codex-parent-thread-id",
    "x-openai-subagent",
];

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OpeningAuditSnapshot {
    pub request_line: String,
    pub headers: Vec<OpeningAuditHeader>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OpeningAuditHeader {
    pub name: String,
    pub value: String,
}

struct OpeningRequestHead {
    request_line: String,
    headers: Vec<OpeningHeader>,
}

struct OpeningHeader {
    name: String,
    value: Vec<u8>,
}

pub(super) async fn connect_with_original_opening_handshake(
    mut request: WsRequest<()>,
) -> Result<(CodexWsStream, WsResponse, OpeningAuditSnapshot), WsError> {
    ensure_websocket_key(&mut request)?;
    let audit_snapshot = websocket_opening_audit_snapshot(&request)?;
    let uri = request.uri().clone();
    let mut stream = connect_stream(&uri).await?;
    let request_bytes = opening_request_bytes(&request)?;

    stream
        .write_all(&request_bytes)
        .await
        .map_err(WsError::Io)?;
    stream.flush().await.map_err(WsError::Io)?;

    let (response, buffered) = read_opening_response(&mut stream).await?;
    if response.status() != StatusCode::SWITCHING_PROTOCOLS {
        return Err(WsError::Http(Box::new(response)));
    }
    verify_accept_key(&request, &response)?;
    let permessage_deflate = response_accepts_permessage_deflate(&response);

    let stream = PerMessageDeflateStream::new(stream, permessage_deflate, buffered);
    let websocket =
        WebSocketStream::from_partially_read(stream, Vec::new(), Role::Client, None).await;
    Ok((websocket, response, audit_snapshot))
}

pub fn websocket_opening_audit_snapshot(
    request: &WsRequest<()>,
) -> Result<OpeningAuditSnapshot, WsError> {
    let head = opening_request_head(request)?;
    Ok(OpeningAuditSnapshot {
        request_line: head.request_line,
        headers: head
            .headers
            .into_iter()
            .map(|header| OpeningAuditHeader {
                value: audit_header_value(&header.name, &header.value),
                name: header.name,
            })
            .collect(),
    })
}

fn ensure_websocket_key(request: &mut WsRequest<()>) -> Result<(), WsError> {
    if request.headers().contains_key("Sec-WebSocket-Key") {
        return Ok(());
    }

    request
        .headers_mut()
        .insert("Sec-WebSocket-Key", HeaderValue::from_str(&generate_key())?);
    Ok(())
}

async fn connect_stream(uri: &Uri) -> Result<MaybeTlsStream<TcpStream>, WsError> {
    let host = uri.host().ok_or(UrlError::NoHostName)?;
    if host.is_empty() {
        return Err(UrlError::EmptyHostName.into());
    }
    let port = uri
        .port_u16()
        .or_else(|| match uri.scheme_str() {
            Some("wss") => Some(443),
            Some("ws") => Some(80),
            _ => None,
        })
        .ok_or(UrlError::UnsupportedUrlScheme)?;

    let socket = TcpStream::connect((host, port))
        .await
        .map_err(WsError::Io)?;

    match uri.scheme_str() {
        Some("ws") => Ok(MaybeTlsStream::Plain(socket)),
        Some("wss") => {
            let server_name = ServerName::try_from(host.to_string())
                .map_err(|_| WsError::Tls(TlsError::InvalidDnsName))?;
            let config = match maybe_build_rustls_client_config_with_custom_ca()
                .map_err(|err| WsError::Io(err.into()))?
            {
                Some(config) => config,
                None => Arc::new(
                    rustls::ClientConfig::builder()
                        .with_root_certificates(native_root_store().map_err(WsError::Io)?)
                        .with_no_client_auth(),
                ),
            };
            let connector = TlsConnector::from(config);
            let stream = connector
                .connect(server_name, socket)
                .await
                .map_err(WsError::Io)?;
            Ok(MaybeTlsStream::Rustls(stream))
        }
        _ => Err(UrlError::UnsupportedUrlScheme.into()),
    }
}

fn opening_request_bytes(request: &WsRequest<()>) -> Result<Vec<u8>, WsError> {
    let head = opening_request_head(request)?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(head.request_line.as_bytes());
    bytes.extend_from_slice(b"\r\n");
    for header in head.headers {
        write_header(&mut bytes, &header.name, &header.value);
    }
    bytes.extend_from_slice(b"\r\n");
    Ok(bytes)
}

fn opening_request_head(request: &WsRequest<()>) -> Result<OpeningRequestHead, WsError> {
    let path = request
        .uri()
        .path_and_query()
        .ok_or(UrlError::NoPathOrQuery)?
        .as_str();
    let host = host_header(request.uri())?;
    let key = request
        .headers()
        .get("Sec-WebSocket-Key")
        .ok_or_else(|| ProtocolError::MissingSecWebSocketKey)?
        .as_bytes();

    let mut headers = Vec::new();
    push_header(&mut headers, "Host", host.as_bytes());
    push_header(&mut headers, "Connection", b"Upgrade");
    push_header(&mut headers, "Upgrade", b"websocket");
    push_header(&mut headers, "Sec-WebSocket-Version", b"13");
    push_header(&mut headers, "Sec-WebSocket-Key", key);
    push_original_business_headers(&mut headers, request.headers());
    push_header(
        &mut headers,
        "sec-websocket-extensions",
        ORIGINAL_WS_PERMESSAGE_DEFLATE_EXTENSION.as_bytes(),
    );
    Ok(OpeningRequestHead {
        request_line: format!("GET {path} HTTP/1.1"),
        headers,
    })
}

fn host_header(uri: &Uri) -> Result<String, WsError> {
    let host = uri.host().ok_or(UrlError::NoHostName)?;
    if host.is_empty() {
        return Err(UrlError::EmptyHostName.into());
    }
    let Some(port) = uri.port_u16() else {
        return Ok(host.to_string());
    };
    let is_default = matches!(
        (uri.scheme_str(), port),
        (Some("ws"), 80) | (Some("wss"), 443)
    );
    if is_default {
        Ok(host.to_string())
    } else {
        Ok(format!("{host}:{port}"))
    }
}

fn push_original_business_headers(output: &mut Vec<OpeningHeader>, headers: &HeaderMap) {
    let mut emitted = Vec::new();
    for name in ORIGINAL_WS_HEADER_ORDER {
        if *name == "session_id" {
            if push_session_headers(output, headers) {
                emitted.push("session_id");
                emitted.push("session-id");
                if headers.get("thread-id").is_none() {
                    emitted.push("thread-id");
                }
            }
        } else if push_named_header(output, headers, name) {
            emitted.push(*name);
        }
    }

    for (name, value) in headers {
        let lower = name.as_str();
        if emitted.iter().any(|seen| seen.eq_ignore_ascii_case(lower))
            || should_skip_websocket_business_header(lower)
        {
            continue;
        }
        push_header(output, original_header_name(lower), value.as_bytes());
    }
}

fn push_named_header(output: &mut Vec<OpeningHeader>, headers: &HeaderMap, name: &str) -> bool {
    let Some(value) = headers.get(name) else {
        return false;
    };
    push_header(output, original_header_name(name), value.as_bytes());
    true
}

fn push_session_headers(output: &mut Vec<OpeningHeader>, headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get("session_id")
        .or_else(|| headers.get("session-id"))
    else {
        return false;
    };

    push_header(output, "session-id", value.as_bytes());
    if headers.get("thread-id").is_none() {
        push_header(output, "thread-id", value.as_bytes());
    }
    true
}

fn push_header(output: &mut Vec<OpeningHeader>, name: &str, value: &[u8]) {
    output.push(OpeningHeader {
        name: name.to_string(),
        value: value.to_vec(),
    });
}

fn write_header(bytes: &mut Vec<u8>, name: &str, value: &[u8]) {
    bytes.extend_from_slice(name.as_bytes());
    bytes.extend_from_slice(b": ");
    bytes.extend_from_slice(value);
    bytes.extend_from_slice(b"\r\n");
}

fn is_opening_header(name: &str) -> bool {
    matches!(
        name,
        "host"
            | "connection"
            | "upgrade"
            | "sec-websocket-version"
            | "sec-websocket-extensions"
            | "sec-websocket-key"
    )
}

fn should_skip_websocket_business_header(name: &str) -> bool {
    is_opening_header(name)
        || matches!(
            name,
            "content-type"
                | "accept"
                | "cookie"
                | "sec-ch-ua"
                | "sec-ch-ua-mobile"
                | "sec-ch-ua-platform"
                | "accept-encoding"
                | "accept-language"
                | "sec-fetch-site"
                | "sec-fetch-mode"
                | "sec-fetch-dest"
                | "x-openai-internal-codex-residency"
                | "x-codex-installation-id"
        )
}

fn original_header_name(name: &str) -> &str {
    match name {
        "authorization" => "authorization",
        "chatgpt-account-id" => "chatgpt-account-id",
        "user-agent" => "user-agent",
        "accept-encoding" => "Accept-Encoding",
        "accept-language" => "Accept-Language",
        "openai-beta" => "openai-beta",
        "cookie" => "Cookie",
        "content-type" => "Content-Type",
        "accept" => "Accept",
        _ => name,
    }
}

fn audit_header_value(name: &str, value: &[u8]) -> String {
    if is_sensitive_audit_header(name) {
        return REDACTED_VALUE.to_string();
    }
    String::from_utf8_lossy(value).into_owned()
}

fn is_sensitive_audit_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization"
            | "chatgpt-account-id"
            | "cookie"
            | "x-client-request-id"
            | "x-codex-installation-id"
            | "session_id"
            | "session-id"
            | "thread-id"
            | "x-codex-window-id"
            | "x-codex-turn-state"
            | "x-codex-turn-metadata"
            | "x-codex-parent-thread-id"
    )
}

async fn read_opening_response(
    stream: &mut MaybeTlsStream<TcpStream>,
) -> Result<(WsResponse, Vec<u8>), WsError> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut chunk).await.map_err(WsError::Io)?;
        if read == 0 {
            return Err(ProtocolError::HandshakeIncomplete.into());
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_OPENING_RESPONSE_BYTES {
            return Err(CapacityError::MessageTooLong {
                size: bytes.len(),
                max_size: MAX_OPENING_RESPONSE_BYTES,
            }
            .into());
        }
        if let Some(index) = find_header_end(&bytes) {
            break index;
        }
    };

    let body_start = header_end + 4;
    let mut body = bytes[body_start..].to_vec();
    let mut response = parse_response_head(&bytes[..header_end])?;
    if response.status() != StatusCode::SWITCHING_PROTOCOLS {
        read_response_body(stream, response.headers(), &mut body).await?;
        *response.body_mut() = Some(body);
        return Ok((response, Vec::new()));
    }

    Ok((response, body))
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_response_head(head: &[u8]) -> Result<WsResponse, WsError> {
    let head = std::str::from_utf8(head)?;
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "empty response"))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "missing response status")
        })?
        .parse::<u16>()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;

    let mut response = WsResponse::new(None);
    *response.status_mut() = StatusCode::from_u16(status)?;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        response.headers_mut().append(
            HeaderName::from_bytes(name.trim().as_bytes())?,
            HeaderValue::from_bytes(value.trim_start().as_bytes())?,
        );
    }
    Ok(response)
}

async fn read_response_body(
    stream: &mut MaybeTlsStream<TcpStream>,
    headers: &HeaderMap,
    body: &mut Vec<u8>,
) -> Result<(), WsError> {
    let Some(content_length) = headers
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return Ok(());
    };

    while body.len() < content_length {
        let remaining = content_length - body.len();
        let mut chunk = vec![0_u8; remaining.min(1024)];
        let read = stream.read(&mut chunk).await.map_err(WsError::Io)?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Ok(())
}

fn verify_accept_key(request: &WsRequest<()>, response: &WsResponse) -> Result<(), WsError> {
    let key = request
        .headers()
        .get("Sec-WebSocket-Key")
        .ok_or(ProtocolError::MissingSecWebSocketKey)?;
    let expected = derive_accept_key(key.as_bytes());
    let actual = response
        .headers()
        .get("Sec-WebSocket-Accept")
        .ok_or(ProtocolError::SecWebSocketAcceptKeyMismatch)?;

    if actual == expected.as_str() {
        Ok(())
    } else {
        Err(ProtocolError::SecWebSocketAcceptKeyMismatch.into())
    }
}

fn response_accepts_permessage_deflate(response: &WsResponse) -> bool {
    response
        .headers()
        .get_all("sec-websocket-extensions")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| {
            value
                .split(',')
                .any(|extension| extension.trim_start().starts_with("permessage-deflate"))
        })
}
