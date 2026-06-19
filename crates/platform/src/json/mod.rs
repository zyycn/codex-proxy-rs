//! 无业务含义的 JSON 与分页辅助。

mod pagination;

pub use pagination::{clamp_limit, decode_cursor, encode_cursor, Page};

use serde_json::Value;

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
