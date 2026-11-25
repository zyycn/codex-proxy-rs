//! 业务头保留多值字节，账号身份与切号追踪边界只作用于本次出站副本

use base64::{Engine as _, engine::general_purpose::STANDARD};
use gateway_core::{engine::middleware::MiddlewareHeader, operation::Operation};
use gateway_protocol::openai::is_transport_managed_request_header;
use serde_json::Value;

pub(crate) fn request_headers(
    operation: &Operation,
    middleware: Vec<MiddlewareHeader>,
    account_switch: bool,
) -> Vec<MiddlewareHeader> {
    let context = match operation {
        Operation::Generate(request) => request.protocol_payload().context(),
        Operation::CountTokens(request) => request.payload().context(),
        _ => return middleware,
    };
    let mut headers = Vec::new();
    if let Some(entries) = context
        .get("opaque_request_headers")
        .and_then(Value::as_array)
    {
        for entry in entries {
            let Some(pair) = entry.as_array().filter(|pair| pair.len() == 2) else {
                continue;
            };
            let Some(name) = pair[0].as_str() else {
                continue;
            };
            let Some(value) = pair[1]
                .as_str()
                .and_then(|value| STANDARD.decode(value).ok())
            else {
                continue;
            };
            // 与其他 Provider 一致，忽略损坏的不透明上下文，不改写合法头的原始值
            if http::HeaderName::from_bytes(name.as_bytes()).is_ok()
                && http::HeaderValue::from_bytes(&value).is_ok()
            {
                headers.push(MiddlewareHeader::new(name, value.into()));
            }
        }
    }
    headers.extend(middleware);
    if account_switch {
        headers.retain(|header| {
            !matches!(
                header.name().to_ascii_lowercase().as_str(),
                "traceparent" | "tracestate" | "sentry-trace" | "baggage"
            )
        });
    }
    headers
}

pub(super) fn managed_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    is_transport_managed_request_header(&name)
        || matches!(
            name.as_str(),
            "authorization" | "cookie" | "cookie2" | "content-type" | "accept"
            | "user-agent" | "x-api-key" | "x-goog-api-key" | "x-goog-user-project"
            | "x-goog-api-client"
            // 跨协议入口的账号身份不能成为 AGY 账号或客户端身份
            | "chatgpt-account-id" | "chatgpt-project-id" | "openai-organization"
            | "openai-project" | "x-openai-actor-authorization" | "x-codex-installation-id"
            | "x-oai-attestation" | "x-oai-is" | "x-oai-is-update"
            | "originator" | "version"
        )
}
