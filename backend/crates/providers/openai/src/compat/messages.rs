//! 跨协议生成的 Messages fast 请求补充协议头，不改写原生 Messages 请求

use bytes::Bytes;
use gateway_core::engine::middleware::MiddlewareHeader;
use reqwest::header::HeaderMap;

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
