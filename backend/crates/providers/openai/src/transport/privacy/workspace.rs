//! 工作区路径的会话化名、Git 元数据裁剪与预览会话识别

use reqwest::header::HeaderMap;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::Budget;

const NAMES: &[&str] = &[
    "cedar", "maple", "willow", "pine", "birch", "elm", "oak", "ash", "harbor", "river", "meadow",
    "forest", "garden", "valley", "summit", "brook", "atlas", "orbit", "lunar", "solar", "amber",
    "coral", "jade", "pearl", "north", "south", "east", "west", "spring", "summer", "autumn",
    "winter",
];
const KINDS: &[&str] = &[
    "api", "web", "app", "tools", "core", "service", "client", "portal",
];

pub(super) fn anonymize(value: &mut Value, budget: &mut Budget<'_>) -> Result<usize, &'static str> {
    let workspaces = value.as_object().ok_or("工作区映射必须是对象")?;
    if workspaces.is_empty() {
        return Ok(0);
    }
    let session = budget
        .session_scope
        .filter(|s| !s.trim().is_empty())
        .ok_or("工作区匿名化缺少稳定的会话标识")?;
    budget.text(session)?;
    let mut renamed = Map::new();
    for (path, metadata) in workspaces {
        budget.text(path)?;
        let mut metadata = metadata.as_object().ok_or("工作区信息必须是对象")?.clone();
        metadata.remove("associated_remote_urls");
        metadata.remove("latest_git_commit_hash");
        let alias = alias_path(path, session)?;
        if renamed.insert(alias, Value::Object(metadata)).is_some() {
            return Err("工作区匿名化产生重名");
        }
    }
    let count = renamed.len();
    *value = Value::Object(renamed);
    Ok(count)
}

fn alias_path(path: &str, session: &str) -> Result<String, &'static str> {
    let path = path.trim_end_matches(['/', '\\']);
    let absolute = path.starts_with(['/', '\\'])
        || (path.as_bytes().get(1) == Some(&b':')
            && path
                .as_bytes()
                .get(2)
                .is_some_and(|c| matches!(c, b'/' | b'\\')));
    if !absolute {
        return Err("工作区路径必须是绝对路径");
    }
    let split = path.rfind(['/', '\\']).ok_or("工作区路径缺少工程目录")?;
    let mut hash = Sha256::new();
    hash.update(b"codex-workspace-alias-v1\0");
    hash.update((session.len() as u64).to_le_bytes());
    hash.update(session.as_bytes());
    hash.update(path.as_bytes());
    let digest = hash.finalize();
    // 保留客户端的分隔符和父目录层级，用户名与工程名改用化名
    let mut alias = String::new();
    let mut username = false;
    for part in path[..=split].split_inclusive(['/', '\\']) {
        let component = part.trim_end_matches(['/', '\\']);
        if username && !component.is_empty() {
            alias.push_str("user");
            alias.push_str(&part[component.len()..]);
            username = false;
        } else {
            alias.push_str(part);
            username =
                component.eq_ignore_ascii_case("users") || component.eq_ignore_ascii_case("home");
        }
    }
    alias.push_str(NAMES[usize::from(digest[0]) % NAMES.len()]);
    alias.push_str(&format!(
        "-{}-",
        u16::from_le_bytes([digest[6], digest[7]]) % 1000 + 1
    ));
    alias.push_str(KINDS[usize::from(digest[1]) % KINDS.len()]);
    alias.push_str(&format!(
        "-{}",
        u16::from_le_bytes([digest[8], digest[9]]) % 1000 + 1
    ));
    // 短后缀扩大命名空间，多个工程仍逐一校验冲突，禁止覆盖合并
    alias.push('-');
    alias.push_str(&hex::encode(&digest[2..6]));
    Ok(alias)
}

pub(super) fn preview_session(
    body: &Value,
    headers: &HeaderMap,
    metadata: Option<&str>,
) -> Option<String> {
    let headers = headers
        .iter()
        .map(|(name, value)| {
            gateway_core::engine::middleware::MiddlewareHeader::new(
                name.as_str(),
                bytes::Bytes::copy_from_slice(value.as_bytes()),
            )
        })
        .collect::<Vec<_>>();
    let mut context = Map::new();
    if let Some(metadata) = metadata {
        context.insert(
            "turn_metadata".to_owned(),
            Value::String(metadata.to_owned()),
        );
    }
    crate::request_identity::account_session_with_headers(body.as_object()?, &context, &headers)
}
