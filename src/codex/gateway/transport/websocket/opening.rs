use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rustls::{ClientConfig, RootCertStore};
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

const MAX_OPENING_RESPONSE_BYTES: usize = 64 * 1024;
const ORIGINAL_WS_PERMESSAGE_DEFLATE_EXTENSION: &str = "permessage-deflate; client_max_window_bits";

const ORIGINAL_WS_HEADER_ORDER: &[&str] = &[
    "authorization",
    "chatgpt-account-id",
    "originator",
    "user-agent",
    "sec-ch-ua",
    "sec-ch-ua-mobile",
    "sec-ch-ua-platform",
    "accept-encoding",
    "accept-language",
    "sec-fetch-site",
    "sec-fetch-mode",
    "sec-fetch-dest",
    "cookie",
    "openai-beta",
    "x-openai-internal-codex-residency",
    "x-client-request-id",
    "x-codex-installation-id",
    "session_id",
    "x-codex-window-id",
    "x-codex-turn-state",
    "x-codex-turn-metadata",
    "x-codex-beta-features",
    "x-responsesapi-include-timing-metrics",
    "version",
    "x-codex-parent-thread-id",
    "x-openai-subagent",
];

pub(super) async fn connect_with_original_opening_handshake(
    mut request: WsRequest<()>,
) -> Result<(CodexWsStream, WsResponse), WsError> {
    ensure_websocket_key(&mut request)?;
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
    Ok((websocket, response))
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
            let connector = TlsConnector::from(Arc::new(rustls_client_config()?));
            let stream = connector
                .connect(server_name, socket)
                .await
                .map_err(WsError::Io)?;
            Ok(MaybeTlsStream::Rustls(stream))
        }
        _ => Err(UrlError::UnsupportedUrlScheme.into()),
    }
}

fn rustls_client_config() -> Result<ClientConfig, WsError> {
    let mut root_store = RootCertStore::empty();
    let rustls_native_certs::CertificateResult { certs, errors, .. } =
        rustls_native_certs::load_native_certs();
    if !errors.is_empty() {
        return Err(WsError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("failed to load native root certificates: {errors:?}"),
        )));
    }

    let (added, _) = root_store.add_parsable_certificates(certs);
    if added == 0 {
        return Err(WsError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no native root certificates found",
        )));
    }

    Ok(ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth())
}

fn opening_request_bytes(request: &WsRequest<()>) -> Result<Vec<u8>, WsError> {
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

    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("GET {path} HTTP/1.1\r\n").as_bytes());
    write_header(&mut bytes, "Host", host.as_bytes());
    write_header(&mut bytes, "Connection", b"Upgrade");
    write_header(&mut bytes, "Upgrade", b"websocket");
    write_header(&mut bytes, "Sec-WebSocket-Version", b"13");
    write_header(
        &mut bytes,
        "Sec-WebSocket-Extensions",
        ORIGINAL_WS_PERMESSAGE_DEFLATE_EXTENSION.as_bytes(),
    );
    write_header(&mut bytes, "Sec-WebSocket-Key", key);
    write_original_business_headers(&mut bytes, request.headers());
    bytes.extend_from_slice(b"\r\n");
    Ok(bytes)
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

fn write_original_business_headers(bytes: &mut Vec<u8>, headers: &HeaderMap) {
    let mut emitted = Vec::new();
    for name in ORIGINAL_WS_HEADER_ORDER {
        if write_named_header(bytes, headers, name) {
            emitted.push(*name);
        }
    }

    for (name, value) in headers {
        let lower = name.as_str();
        if emitted.iter().any(|seen| seen.eq_ignore_ascii_case(lower))
            || is_opening_header(lower)
            || lower == "content-type"
            || lower == "accept"
        {
            continue;
        }
        write_header(bytes, original_header_name(lower), value.as_bytes());
    }
}

fn write_named_header(bytes: &mut Vec<u8>, headers: &HeaderMap, name: &str) -> bool {
    let Some(value) = headers.get(name) else {
        return false;
    };
    write_header(bytes, original_header_name(name), value.as_bytes());
    true
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

fn original_header_name(name: &str) -> &str {
    match name {
        "authorization" => "Authorization",
        "chatgpt-account-id" => "ChatGPT-Account-Id",
        "user-agent" => "User-Agent",
        "accept-encoding" => "Accept-Encoding",
        "accept-language" => "Accept-Language",
        "openai-beta" => "OpenAI-Beta",
        "cookie" => "Cookie",
        "content-type" => "Content-Type",
        "accept" => "Accept",
        _ => name,
    }
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
