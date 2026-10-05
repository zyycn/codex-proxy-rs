//! Codex Live / Realtime 语音通话的协议 adapter。
//!
//! 通话引导走 Provider HTTP 端点通道（保留客户端账号范围、准入与请求记录）；
//! 通话建立后的 sideband 中继走 [`gateway_core::live::LiveGateway`] 的钉住
//! 账号拨号。上游合同对应官方 Codex 的 realtime_call 与 realtime_websocket：
//! `POST /v1/live` 引导 WebRTC SDP，`GET /v1/live/{call_id}` 提供事件 sideband。

mod http;
mod websocket;

use axum::{
    Router,
    body::Bytes,
    extract::DefaultBodyLimit,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use gateway_core::event::ProviderResponseHeader;
use gateway_core::operation::{
    ProviderHttpHeader, ProviderHttpMethod, ProviderHttpRequest, RawHttpPayload,
};
use serde_json::{Value, json};

use crate::openai::error::openai_error_response;

pub(crate) use http::{hangup, live_call};
pub(crate) use websocket::{realtime_get, sideband};

/// 语音引导正文上限，同时约束 JSON、SDP 与 multipart 入口。
pub(crate) const MAX_LIVE_BODY_BYTES: usize = 16 * 1024 * 1024;
/// Provider 侧识别 realtime calls 端点的符号名；endpoint 字段只允许
/// 单段安全符号，真实上游路径 `/codex/realtime/calls` 由 Provider 映射。
pub(crate) const LIVE_CALLS_ENDPOINT: &str = "realtime-calls";
/// realtime calls 的官方查询参数；上游按 `architecture=avas` 返回 WebRTC answer。
pub(crate) const LIVE_CALLS_QUERY: &str = "intent=quicksilver&architecture=avas";
/// 客户端协议头允许清单；语音会话语义字段，转发给上游引导请求。
pub(crate) const LIVE_PROTOCOL_HEADERS: [&str; 9] = [
    "openai-alpha",
    "x-session-id",
    "session-id",
    "thread-id",
    "originator",
    "openai-safety-identifier",
    "openai-organization",
    "openai-project",
    "x-oai-attestation",
];

pub(crate) fn router() -> Router<crate::ApiState> {
    Router::new()
        .route("/v1/live", post(live_call))
        .route("/v1/live/{call_id}", get(sideband))
        .route("/v1/realtime", post(live_call).get(realtime_get))
        .route("/v1/realtime/calls", post(live_call))
        .route("/v1/realtime/calls/{call_id}", get(sideband))
        .route("/v1/realtime/calls/{call_id}/hangup", post(hangup))
        // Codex OAuth 上游与本网关都不提供这些 realtime 能力；显式 501 优于神秘 404。
        .route("/v1/realtime/translations", any(translation_unsupported))
        .route(
            "/v1/realtime/translations/client_secrets",
            any(translation_unsupported),
        )
        .route(
            "/v1/realtime/transcription_sessions",
            post(transcription_unsupported),
        )
        .route(
            "/v1/realtime/client_secrets",
            post(client_secrets_unsupported),
        )
        .route("/v1/realtime/sessions", post(legacy_sessions_unsupported))
        .route("/v1/realtime/calls/{call_id}/accept", post(sip_unsupported))
        .route("/v1/realtime/calls/{call_id}/reject", post(sip_unsupported))
        .route("/v1/realtime/calls/{call_id}/refer", post(sip_unsupported))
        .layer(DefaultBodyLimit::max(MAX_LIVE_BODY_BYTES))
}

/// `/v1/live` 与 `/v1/realtime*` 的错误投影差异：前者是普通 JSON 错误对象，
/// realtime 家族沿用 OpenAI realtime 的结构化错误合同。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveErrorShape {
    Live,
    Realtime,
}

pub(crate) fn live_error_response(
    shape: LiveErrorShape,
    status: StatusCode,
    message: &str,
    code: &str,
) -> Response {
    match shape {
        LiveErrorShape::Live => {
            let body = json!({ "error": message });
            (status, axum::Json(body)).into_response()
        }
        LiveErrorShape::Realtime => {
            openai_error_response(status, message, error_type_for_status(status), code)
                .into_response()
        }
    }
}

fn error_type_for_status(status: StatusCode) -> &'static str {
    if status.is_client_error() {
        "invalid_request_error"
    } else {
        "api_error"
    }
}

async fn translation_unsupported() -> Response {
    realtime_unsupported_response("Realtime translation sessions")
}

async fn transcription_unsupported() -> Response {
    realtime_unsupported_response("Realtime transcription-only sessions")
}

async fn client_secrets_unsupported() -> Response {
    realtime_unsupported_response("Realtime client secrets")
}

async fn legacy_sessions_unsupported() -> Response {
    realtime_unsupported_response("Legacy realtime session credentials")
}

async fn sip_unsupported() -> Response {
    realtime_unsupported_response("Realtime SIP control")
}

pub(crate) fn realtime_unsupported_response(capability: &str) -> Response {
    openai_error_response(
        StatusCode::NOT_IMPLEMENTED,
        &format!("{capability} are not supported by the Codex OAuth upstream"),
        "not_supported_error",
        "realtime_capability_not_supported",
    )
    .into_response()
}

/// 请求侧协议头过滤；语音语义字段按允许清单转发，其余一律不透传。
pub(crate) fn filter_protocol_headers(headers: &HeaderMap) -> Vec<ProviderHttpHeader> {
    LIVE_PROTOCOL_HEADERS
        .iter()
        .flat_map(|name| {
            headers
                .get_all(*name)
                .into_iter()
                .map(|value| (*name, value))
        })
        .filter_map(|(name, value)| {
            let value = value.to_str().ok()?;
            Some(ProviderHttpHeader::new(
                name,
                Bytes::copy_from_slice(value.as_bytes()),
            ))
        })
        .collect()
}

/// 客户端 offer 的 WebSocket 子协议，按出现顺序透传给上游协商。
pub(crate) fn offered_subprotocols(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .into_iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// 把协议头与正文装配为 realtime calls 引导 operation。
pub(crate) fn live_call_operation(
    body: Bytes,
    content_type: Option<&str>,
    protocol_headers: Vec<ProviderHttpHeader>,
) -> Result<ProviderHttpRequest, Box<Response>> {
    let mut headers = protocol_headers;
    if let Some(content_type) = content_type {
        headers.push(ProviderHttpHeader::new(
            "content-type",
            Bytes::copy_from_slice(content_type.as_bytes()),
        ));
    }
    let invalid = || {
        Box::new(live_error_response(
            LiveErrorShape::Live,
            StatusCode::BAD_REQUEST,
            "Codex live call request is invalid",
            "invalid_request",
        ))
    };
    let payload = RawHttpPayload::new("openai", body).map_err(|_| invalid())?;
    ProviderHttpRequest::new(
        LIVE_CALLS_ENDPOINT,
        ProviderHttpMethod::Post,
        Some(LIVE_CALLS_QUERY.to_owned()),
        headers,
        payload,
    )
    .map_err(|_| invalid())
}

/// 从 `{"sdp": …, "session"?: …}` 引导体提取模型名；`session.model` 优先。
pub(crate) fn live_requested_model(body: &[u8]) -> Option<String> {
    let payload = serde_json::from_slice::<Value>(body).ok()?;
    let extract = |value: Option<&Value>| -> Option<String> {
        value
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(ToOwned::to_owned)
    };
    extract(payload.pointer("/session/model")).or_else(|| extract(payload.get("model")))
}

/// realtime 系模型名归一到 Codex 语音模型；其余模型原样透传。
pub(crate) fn codex_realtime_model(model: Option<String>) -> String {
    const DEFAULT_LIVE_MODEL: &str = "gpt-live-1-codex";
    let Some(model) = model else {
        return DEFAULT_LIVE_MODEL.to_owned();
    };
    let trimmed = model.trim();
    let lowered = trimmed.to_ascii_lowercase();
    if lowered.is_empty()
        || lowered == "gpt-realtime"
        || lowered.starts_with("gpt-realtime-")
        || lowered.contains("realtime-preview")
    {
        return DEFAULT_LIVE_MODEL.to_owned();
    }
    trimmed.to_owned()
}

/// 按官方合同的观测，把归一后的模型写回引导体；无模型字段时不改写。
pub(crate) fn rewrite_call_request_model(body: Bytes, upstream_model: &str) -> Bytes {
    let Ok(mut payload) = serde_json::from_slice::<serde_json::Map<String, Value>>(&body) else {
        return body;
    };
    let mut changed = false;
    if let Some(session) = payload.get_mut("session").filter(|s| !s.is_null())
        && let Some(session) = session.as_object_mut()
    {
        session.insert("model".to_owned(), Value::String(upstream_model.to_owned()));
        changed = true;
    } else if payload.contains_key("model") {
        payload.insert("model".to_owned(), Value::String(upstream_model.to_owned()));
        changed = true;
    }
    if !changed {
        return body;
    }
    serde_json::to_vec(&payload)
        .map(Bytes::from)
        .unwrap_or(body)
}

/// 把 SDP 文本包装为上游要求的 JSON 引导体。
pub(crate) fn encode_call_request(
    sdp: &str,
    session: Option<Value>,
) -> Result<Bytes, Box<Response>> {
    let invalid = |message: &'static str| {
        Box::new(live_error_response(
            LiveErrorShape::Live,
            StatusCode::BAD_REQUEST,
            message,
            "invalid_request",
        ))
    };
    if sdp.trim().is_empty() {
        return Err(invalid("Codex live call request requires an SDP offer"));
    }
    let mut payload = serde_json::Map::new();
    payload.insert("sdp".to_owned(), Value::String(sdp.to_owned()));
    if let Some(session) = session.filter(|session| !session.is_null()) {
        payload.insert("session".to_owned(), session);
    }
    serde_json::to_vec(&payload)
        .map(Bytes::from)
        .map_err(|_| invalid("Codex live call request is invalid"))
}

/// 响应头白名单；`Location` 是客户端获知 call id 的合同。
pub(crate) fn live_forwarded_response_headers(
    headers: &[ProviderResponseHeader],
) -> Vec<(HeaderName, HeaderValue)> {
    const FORWARDABLE: [&str; 5] = [
        "location",
        "content-type",
        "retry-after",
        "x-request-id",
        "openai-request-id",
    ];
    headers
        .iter()
        .filter(|header| FORWARDABLE.contains(&header.name()))
        .filter_map(|header| {
            Some((
                HeaderName::from_bytes(header.name().as_bytes()).ok()?,
                HeaderValue::from_bytes(header.value()).ok()?,
            ))
        })
        .collect()
}

/// 引导体标准化：JSON 原样、SDP 文本包装、multipart 字段（sdp+session）重组。
pub(crate) fn normalize_call_request(
    body: &Bytes,
    content_type: Option<&str>,
    multipart: Option<(String, Option<Value>)>,
) -> Result<(Bytes, Option<String>, Option<String>), Box<Response>> {
    if let Some((sdp, session)) = multipart {
        let model = session
            .as_ref()
            .and_then(|session| serde_json::to_vec(session).ok())
            .and_then(|encoded| live_requested_model(&encoded));
        let encoded = encode_call_request(&sdp, session)?;
        return Ok((encoded, Some("application/json".to_owned()), model));
    }
    let media_type = content_type
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    match media_type.as_deref() {
        Some("application/sdp") | Some("text/plain") => {
            let sdp = std::str::from_utf8(body).map_err(|_| {
                Box::new(live_error_response(
                    LiveErrorShape::Live,
                    StatusCode::BAD_REQUEST,
                    "Codex live call request requires a UTF-8 SDP offer",
                    "invalid_request",
                ))
            })?;
            let encoded = encode_call_request(sdp, None)?;
            Ok((encoded, Some("application/json".to_owned()), None))
        }
        _ => {
            let model = live_requested_model(body);
            let content_type = content_type
                .map(ToOwned::to_owned)
                .or_else(|| Some("application/json".to_owned()));
            Ok((body.clone(), content_type, model))
        }
    }
}
