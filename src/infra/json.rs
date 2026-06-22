//! 无业务含义的 JSON 与分页辅助。

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::Serialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// 分页
// ---------------------------------------------------------------------------

/// 通用分页结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    /// 当前页条目。
    pub items: Vec<T>,
    /// 下一页游标。
    pub next_cursor: Option<String>,
}

/// 编码游标。
pub fn encode_cursor(created_at: &str, id: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("{created_at}|{id}"))
}

/// 解码游标。
pub fn decode_cursor(cursor: &str) -> Option<(String, String)> {
    let raw = URL_SAFE_NO_PAD.decode(cursor).ok()?;
    let text = String::from_utf8(raw).ok()?;
    let (created_at, id) = text.split_once('|')?;
    Some((created_at.to_string(), id.to_string()))
}

/// 限制分页大小。
pub fn clamp_limit(limit: u32) -> u32 {
    limit.clamp(1, 200)
}

// ---------------------------------------------------------------------------
// JSON 值提取
// ---------------------------------------------------------------------------

/// 从多个路径中提取第一个非空字符串。
pub fn first_string(value: &Value, paths: &[&[&str]]) -> Option<String> {
    paths.iter().find_map(|path| string_at(value, path))
}

/// 从指定路径提取字符串。
pub fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    current
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}
