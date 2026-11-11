//! Messages 跨协议转换的目标档位与 fast 协议头适配

use bytes::Bytes;
use gateway_core::engine::middleware::MiddlewareHeader;
use reqwest::header::HeaderMap;
use serde_json::{Map, Value};

pub(crate) fn normalize_translated_codex_service_tier(body: &mut Map<String, Value>) {
    // 转换生成的默认档位沿用 Codex 客户端的省略表达，原生 Responses 保留调用方声明
    if matches!(
        body.get("service_tier").and_then(Value::as_str),
        Some("auto" | "default")
    ) {
        body.shift_remove("service_tier");
    }
}

pub(crate) fn add_translated_fast_beta(headers: &mut Vec<MiddlewareHeader>, forwarded: &HeaderMap) {
    const BETA: &[u8] = b"fast-mode-2026-02-01";
    let values = if headers
        .iter()
        .any(|header| header.name().eq_ignore_ascii_case("anthropic-beta"))
    {
        headers
            .iter()
            .filter(|header| header.name().eq_ignore_ascii_case("anthropic-beta"))
            .map(|header| header.value().as_ref())
            .collect::<Vec<_>>()
    } else {
        forwarded
            .get_all("anthropic-beta")
            .iter()
            .map(|value| value.as_bytes())
            .collect()
    };
    let mut joined = Vec::new();
    for value in values {
        if !joined.is_empty() {
            joined.push(b',');
        }
        joined.extend_from_slice(value);
    }
    if !joined
        .split(|byte| *byte == b',')
        .any(|token| token.trim_ascii() == BETA)
    {
        if !joined.is_empty() {
            joined.push(b',');
        }
        joined.extend_from_slice(BETA);
    }
    // 出站中间件头优先于透传头，合并后一次写入，保留调用方已有 beta
    headers.retain(|header| !header.name().eq_ignore_ascii_case("anthropic-beta"));
    headers.push(MiddlewareHeader::new("anthropic-beta", Bytes::from(joined)));
}
