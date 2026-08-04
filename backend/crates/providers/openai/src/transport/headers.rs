use crate::transport::profile::CodexWireProfile;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue, USER_AGENT};

use super::client::CodexClientResult;

/// 构造 Codex Desktop HTTP 为模型请求设置的稳定身份请求头。
pub fn build_codex_base_headers(
    profile: &CodexWireProfile,
    authorization: &str,
    account_id: Option<&str>,
) -> CodexClientResult<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_str(authorization)?);
    insert_optional_header(&mut headers, "chatgpt-account-id", account_id)?;
    headers.insert(
        HeaderName::from_static("originator"),
        HeaderValue::from_str(&profile.originator)?,
    );
    headers.insert(USER_AGENT, HeaderValue::from_str(&profile.user_agent())?);
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

/// 尽力投影客户端协议值；无法表示为 HTTP header 时保留正文并跳过投影。
pub(super) fn insert_optional_protocol_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: Option<&str>,
) {
    let Some(value) = value.and_then(|value| HeaderValue::from_str(value).ok()) else {
        return;
    };
    headers.insert(HeaderName::from_static(name), value);
}

fn header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        // tungstenite 的 opening serializer 只接受可 `to_str()` 的值；逐条跳过
        // 无法构造的头，不能让一个扩展头中断业务 payload。
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}

pub(super) fn websocket_header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    header_pairs(headers)
}
