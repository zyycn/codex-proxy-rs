use indexmap::IndexMap;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use crate::codex::fingerprint::Fingerprint;

use super::client::CodexClientResult;

/// 构造标准 Codex 请求头。
pub fn build_codex_base_headers(
    fingerprint: &Fingerprint,
    access_token: &str,
    account_id: Option<&str>,
) -> IndexMap<String, String> {
    let mut headers = IndexMap::new();
    headers.insert(
        "authorization".to_string(),
        format!("Bearer {access_token}"),
    );
    if let Some(account_id) = account_id {
        headers.insert("chatgpt-account-id".to_string(), account_id.to_string());
    }
    headers.insert("originator".to_string(), fingerprint.originator.clone());
    headers.insert("user-agent".to_string(), fingerprint.user_agent());
    headers.insert("sec-ch-ua".to_string(), fingerprint.sec_ch_ua());
    for (key, value) in &fingerprint.default_headers {
        let key_lower = key.to_ascii_lowercase();
        if !headers.contains_key(&key_lower) {
            headers.insert(key_lower, value.clone());
        }
    }
    headers
}

/// 构造包含标准请求头和额外请求头值的完整请求头集合。
pub fn build_codex_headers(
    fingerprint: &Fingerprint,
    access_token: &str,
    account_id: Option<&str>,
    turn_state: Option<&str>,
    request_id: &str,
) -> IndexMap<String, String> {
    let mut headers = build_codex_base_headers(fingerprint, access_token, account_id);
    headers.insert(
        "x-openai-internal-codex-residency".to_string(),
        "us".to_string(),
    );
    headers.insert("x-client-request-id".to_string(), request_id.to_string());
    if let Some(turn_state) = turn_state {
        headers.insert("x-codex-turn-state".to_string(), turn_state.to_string());
    }
    headers.insert("accept".to_string(), "text/event-stream".to_string());
    headers
}

/// 按给定顺序重排请求头。
pub fn order_headers(
    headers: IndexMap<String, String>,
    order: &[String],
) -> IndexMap<String, String> {
    let mut ordered = IndexMap::new();
    for key in order {
        if let Some(value) = headers.get(key) {
            ordered.insert(key.clone(), value.clone());
        }
    }
    for (key, value) in headers {
        if !ordered.contains_key(&key) {
            ordered.insert(key, value);
        }
    }
    ordered
}

/// 构造并按指纹顺序重排完整请求头。
pub fn build_ordered_codex_headers(
    fingerprint: &Fingerprint,
    access_token: &str,
    account_id: Option<&str>,
    turn_state: Option<&str>,
    request_id: &str,
) -> IndexMap<String, String> {
    let headers = build_codex_headers(
        fingerprint,
        access_token,
        account_id,
        turn_state,
        request_id,
    );
    order_headers(headers, &fingerprint.header_order)
}

/// 构造并按指纹顺序重排基础请求头（不含请求 ID 和 turn state）。
pub fn build_ordered_codex_base_headers(
    fingerprint: &Fingerprint,
    access_token: &str,
    account_id: Option<&str>,
) -> IndexMap<String, String> {
    let headers = build_codex_base_headers(fingerprint, access_token, account_id);
    order_headers(headers, &fingerprint.header_order)
}

pub(super) fn insert_optional_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: Option<&str>,
) -> CodexClientResult<()> {
    let Some(value) = value else {
        return Ok(());
    };
    headers.insert(HeaderName::from_static(name), HeaderValue::from_str(value)?);
    Ok(())
}

pub(super) fn insert_ordered_headers(
    headers: &mut HeaderMap,
    ordered_headers: &IndexMap<String, String>,
) -> CodexClientResult<()> {
    for (name, value) in ordered_headers {
        headers.insert(
            HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }
    Ok(())
}

fn header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect()
}

pub(super) fn websocket_header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    header_pairs(headers)
        .into_iter()
        .filter(|(name, _)| {
            !name.eq_ignore_ascii_case("content-type") && !name.eq_ignore_ascii_case("accept")
        })
        .collect::<Vec<_>>()
}
