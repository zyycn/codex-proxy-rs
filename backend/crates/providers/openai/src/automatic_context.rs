//! 自动上下文定位与 Desktop Git XML/JSON 编解码，供画像与隐私规则共用

use serde_json::Value;

pub(crate) struct ContextTarget {
    pub(crate) path: String,
    open: &'static str,
    close: &'static str,
    pub(crate) identified: bool,
}

impl ContextTarget {
    pub(crate) fn is_complete(&self, text: &str) -> bool {
        let trimmed = text.trim();
        trimmed.starts_with(self.open) && trimmed.ends_with(self.close)
    }
}

pub(crate) fn locate_contexts(
    body: &Value,
    desktop: bool,
    mut visit: impl FnMut() -> Result<(), &'static str>,
) -> Result<Vec<ContextTarget>, &'static str> {
    let mut targets = Vec::new();
    let Some(items) = body.get("input").and_then(Value::as_array) else {
        return Ok(targets);
    };
    for (item_index, item) in items.iter().enumerate() {
        visit()?;
        let role = item.get("role").and_then(Value::as_str).unwrap_or("");
        if !(role == "user" || (!desktop && role == "developer")) {
            continue;
        }
        let (kind, open, close) = if desktop {
            (
                "additional_content.codex_git_state",
                "<external_codex_git_state>",
                "</external_codex_git_state>",
            )
        } else if role == "developer" {
            (
                "additional_content.codex_apps_client_time_context",
                "<codex_apps_client_time_context>",
                "</codex_apps_client_time_context>",
            )
        } else {
            (
                "environments.environment_context",
                "<environment_context>",
                "</environment_context>",
            )
        };
        let kinds = item.pointer("/internal_chat_message_metadata_passthrough/content_item_kinds");
        let Some(parts) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for (index, part) in parts.iter().enumerate() {
            visit()?;
            if kinds.is_some_and(|kinds| kinds.get(index).and_then(Value::as_str) != Some(kind)) {
                continue;
            }
            if part.get("type").and_then(Value::as_str) != Some("input_text") {
                continue;
            }
            let Some(text) = part.get("text").and_then(Value::as_str) else {
                continue;
            };
            let trimmed = text.trim();
            if kinds.is_none() && !(trimmed.starts_with(open) && trimmed.ends_with(close)) {
                continue;
            }
            targets.push(ContextTarget {
                path: format!("/input/{item_index}/content/{index}/text"),
                open,
                close,
                identified: kinds.is_some(),
            });
        }
    }
    Ok(targets)
}

pub(crate) fn rewrite_git_context(
    text: &mut String,
    max_bytes: usize,
    edit: impl FnOnce(&mut Value) -> Result<usize, &'static str>,
) -> Result<usize, &'static str> {
    let document = parse_xml(text)?;
    let root = document.root_element();
    if root.tag_name().name() != "external_codex_git_state" {
        return Err("Desktop Git 包裹不匹配");
    }
    let nodes: Vec<_> = root
        .descendants()
        .filter(|node| {
            node.has_tag_name("git")
                && node
                    .parent()
                    .is_some_and(|p| p.has_tag_name("environment_context"))
        })
        .collect();
    if nodes.len() != 1 {
        return Err("Desktop Git 数据节点不明确");
    }
    let git = nodes[0];
    if git.children().any(|node| !node.is_text()) {
        return Err("Desktop Git 数据不是文本");
    }
    let raw: String = git.children().filter_map(|node| node.text()).collect();
    let mut value: Value = serde_json::from_str(&raw).map_err(|_| "Desktop Git JSON 无法解析")?;
    if !value.is_object() {
        return Err("Desktop Git JSON 必须是对象");
    }
    let count = edit(&mut value)?;
    if count > 0 {
        let range = git.range();
        let fragment = &text[range.clone()];
        let start = git
            .first_child()
            .ok_or("Desktop Git 数据为空")?
            .range()
            .start;
        let end = range.start + fragment.rfind("</").ok_or("Desktop Git 包裹无效")?;
        let encoded = serde_json::to_string(&value).map_err(|_| "Desktop Git JSON 无法编码")?;
        let escaped = encoded
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        if text.len() - (end - start) + escaped.len() > max_bytes {
            return Err("Desktop Git 编码结果超出大小限制");
        }
        text.replace_range(start..end, &escaped);
    }
    Ok(count)
}

pub(crate) fn parse_xml(text: &str) -> Result<roxmltree::Document<'_>, &'static str> {
    roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            nodes_limit: 10_000,
            ..Default::default()
        },
    )
    .map_err(|_| "上下文 XML 无法解析或节点数量超限")
}
