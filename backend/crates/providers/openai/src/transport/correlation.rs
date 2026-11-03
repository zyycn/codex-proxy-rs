//! 在出站副本中隔离账号的追踪标识，不改变本地亲和、续接引用或业务正文

use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::automatic_context::{locate_contexts, rewrite_git_context};
use super::request::serialize_ascii_turn_metadata;

// 轮次及父子轮次引用保留客户端原值，避免改变官方轮次身份与 UUIDv7 语义
const ID_FIELDS: &[&str] = &[
    "session_id",
    "thread_id",
    "window_id",
    "context_window_id",
    "parent_thread_id",
    "forked_from_thread_id",
    "conversation_id",
    "x-codex-window-id",
    "x-codex-parent-thread-id",
    "x-client-request-id",
    "codexWindowId",
    "parentThreadId",
];
const METADATA_FIELDS: &[&str] = &["turnMetadata", "turn_metadata", "x-codex-turn-metadata"];
// Live 的 x-session-id 与 voice-session-id 是业务会话引用，必须原样传递
const ID_HEADERS: &[&str] = &[
    "session-id",
    "thread-id",
    "x-codex-window-id",
    "x-codex-parent-thread-id",
    "x-client-request-id",
];

fn scoped_id(installation_id: &str, original: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"codex-account-correlation-v1\0");
    digest.update(installation_id.as_bytes());
    digest.update(b"\0");
    digest.update(original.as_bytes());
    let hash = digest.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    // 保持官方 UUID 的形状；各类引用共享映射，保留相同原值的等值关系
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

/// Search 的 id 是会话关联键，仅替换该值的原始字节，保留其他字段、重复键和空白
pub(super) fn scope_search_body(body: bytes::Bytes, installation_id: Option<&str>) -> bytes::Bytes {
    #[derive(serde::Deserialize)]
    struct SearchIdentity<'a> {
        #[serde(borrow)]
        id: Option<&'a serde_json::value::RawValue>,
    }
    let Some(installation_id) = installation_id else {
        return body;
    };
    let Ok(identity) = serde_json::from_slice::<SearchIdentity<'_>>(&body) else {
        return body;
    };
    let Some(raw) = identity.id.map(serde_json::value::RawValue::get) else {
        return body;
    };
    let Ok(original) = serde_json::from_str::<String>(raw) else {
        return body;
    };
    if original.is_empty() {
        return body;
    }
    // from_slice 返回的 RawValue 借用原始正文，跨度不包含相邻键或分隔符
    let offset = raw.as_ptr() as usize - body.as_ptr() as usize;
    let replacement = format!("\"{}\"", scoped_id(installation_id, &original));
    let mut scoped = Vec::with_capacity(body.len() - raw.len() + replacement.len());
    scoped.extend_from_slice(&body[..offset]);
    scoped.extend_from_slice(replacement.as_bytes());
    scoped.extend_from_slice(&body[offset + raw.len()..]);
    scoped.into()
}

fn map_fields(object: &mut Map<String, Value>, installation_id: &str, fields: &[&str]) -> usize {
    let mut changed = 0;
    for key in fields {
        if let Some(Value::String(value)) = object.get_mut(*key)
            && !value.is_empty()
        {
            *value = scoped_id(installation_id, value);
            changed += 1;
        }
    }
    changed
}

fn scope_metadata(value: &mut Value, installation_id: &str) {
    match value {
        Value::Object(object) => {
            map_fields(object, installation_id, ID_FIELDS);
        }
        Value::String(raw) => {
            if let Ok(Value::Object(mut object)) = serde_json::from_str(raw)
                && map_fields(&mut object, installation_id, ID_FIELDS) > 0
                && let Some(encoded) = serialize_ascii_turn_metadata(&Value::Object(object))
            {
                *raw = encoded;
            }
        }
        _ => {}
    }
}

pub(super) fn scope_headers(headers: &mut HeaderMap, installation_id: Option<&str>) {
    let Some(installation_id) = installation_id else {
        return;
    };
    for name in ID_HEADERS {
        let values: Vec<_> = headers
            .get_all(*name)
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .ok()
                    .filter(|value| !value.is_empty())
                    .and_then(|value| {
                        HeaderValue::from_str(&scoped_id(installation_id, value)).ok()
                    })
                    .unwrap_or_else(|| value.clone())
            })
            .collect();
        headers.remove(*name);
        for value in values {
            headers.append(*name, value);
        }
    }
    let values: Vec<_> = headers
        .get_all("x-codex-turn-metadata")
        .iter()
        .map(|value| {
            let Ok(raw) = value.to_str() else {
                return value.clone();
            };
            let mut metadata = Value::String(raw.to_owned());
            scope_metadata(&mut metadata, installation_id);
            metadata
                .as_str()
                .and_then(|raw| HeaderValue::from_str(raw).ok())
                .unwrap_or_else(|| value.clone())
        })
        .collect();
    headers.remove("x-codex-turn-metadata");
    for value in values {
        headers.append("x-codex-turn-metadata", value);
    }
}

pub(super) fn scope_request(
    body: &mut Value,
    headers: &mut HeaderMap,
    installation_id: Option<&str>,
) {
    scope_headers(headers, installation_id);
    let Some(installation_id) = installation_id else {
        return;
    };
    if let Some(object) = body.as_object_mut() {
        map_fields(object, installation_id, ID_FIELDS);
        map_fields(object, installation_id, &["prompt_cache_key"]);
        for key in METADATA_FIELDS {
            if let Some(value) = object.get_mut(*key) {
                scope_metadata(value, installation_id);
            }
        }
        if let Some(Value::Object(metadata)) = object.get_mut("client_metadata") {
            map_fields(metadata, installation_id, ID_FIELDS);
            for key in METADATA_FIELDS {
                if let Some(value) = metadata.get_mut(*key) {
                    scope_metadata(value, installation_id);
                }
            }
        }
    }
    // 只处理官方自动 Git 上下文的关联字段，不扫描普通用户输入和工具输出
    let Ok(targets) = locate_contexts(body, true, || Ok(())) else {
        return;
    };
    for target in targets {
        let Some(Value::String(text)) = body.pointer_mut(&target.path) else {
            continue;
        };
        if text.len() > 1024 * 1024 || !target.is_complete(text) {
            continue;
        }
        let _ = rewrite_git_context(text, 1024 * 1024, |value| {
            Ok(value.as_object_mut().map_or(0, |object| {
                map_fields(object, installation_id, &["sessionId", "threadId"])
            }))
        });
    }
}
