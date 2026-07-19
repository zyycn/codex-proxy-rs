use crate::transport::profile::CodexWireProfile;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderName, HeaderValue, USER_AGENT};

use super::client::CodexClientResult;

/// 构造 Codex Core 为模型请求设置的稳定身份请求头。
pub fn build_codex_base_headers(
    profile: &CodexWireProfile,
    access_token: &str,
    account_id: Option<&str>,
) -> CodexClientResult<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {access_token}"))?,
    );
    insert_optional_header(&mut headers, "chatgpt-account-id", account_id)?;
    headers.insert(
        HeaderName::from_static("originator"),
        HeaderValue::from_str(&profile.originator)?,
    );
    headers.insert(USER_AGENT, HeaderValue::from_str(&profile.user_agent())?);
    Ok(headers)
}

/// 构造含通用请求上下文的 Codex 模型请求头。
pub fn build_codex_headers(
    profile: &CodexWireProfile,
    access_token: &str,
    account_id: Option<&str>,
    turn_state: Option<&str>,
    request_id: &str,
) -> CodexClientResult<HeaderMap> {
    let mut headers = build_codex_base_headers(profile, access_token, account_id)?;
    headers.insert(
        HeaderName::from_static("x-openai-internal-codex-residency"),
        HeaderValue::from_static("us"),
    );
    headers.insert(
        HeaderName::from_static("x-client-request-id"),
        HeaderValue::from_str(request_id)?,
    );
    insert_optional_header(&mut headers, "x-codex-turn-state", turn_state)?;
    headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    Ok(headers)
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
