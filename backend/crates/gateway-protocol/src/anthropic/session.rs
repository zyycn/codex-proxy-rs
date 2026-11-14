//! Claude Code 明确会话身份的请求头与 metadata 解析

use serde_json::{Map, Value};

/// 优先使用 Claude Code 会话头，再读取新旧 metadata.user_id 中的会话标识
///
/// 普通用户标识与对话内容不作为会话身份
#[must_use]
pub fn claude_session_id(body: &Map<String, Value>, header: Option<&str>) -> Option<String> {
    if let Some(session) = header.and_then(valid_session_id) {
        return Some(session.to_owned());
    }
    let user_id = body.get("metadata")?.get("user_id")?.as_str()?.trim();
    if user_id.starts_with('{') {
        let metadata: Value = serde_json::from_str(user_id).ok()?;
        return metadata
            .get("session_id")?
            .as_str()
            .and_then(valid_session_id)
            .map(str::to_owned);
    }
    let (_, session) = user_id.rsplit_once("_session_")?;
    let session = valid_session_id(session)?;
    session
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        .then(|| session.to_owned())
}

fn valid_session_id(value: &str) -> Option<&str> {
    if value.chars().any(char::is_control) {
        return None;
    }
    let value = value.trim();
    (!value.is_empty() && value.len() <= 256).then_some(value)
}
