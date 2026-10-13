//! 对独立 metadata 副本及已识别自动上下文进行解码、改写与原位写回

use gateway_core::settings::privacy::PrivacyScope;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::Value;

use super::{Budget, changes::BodyChanges, rule::CompiledRule};

const METADATA_KEYS: &[&str] = &["turnMetadata", "turn_metadata", "x-codex-turn-metadata"];

pub(super) fn prepare(
    rule: &CompiledRule,
    body: &Value,
    headers: &mut HeaderMap,
    turn_metadata: &mut Option<String>,
    changes: &mut BodyChanges,
    contexts: &mut ContextIndex,
    budget: &mut Budget<'_>,
) -> Result<usize, &'static str> {
    match rule.rule.scope {
        PrivacyScope::TurnMetadata => {
            let mut count = 0;
            if let Some(raw) = turn_metadata {
                count += metadata(rule, raw, budget)?;
            }
            for (name, value) in headers.iter_mut() {
                if name == "x-codex-turn-metadata" {
                    let mut raw = std::str::from_utf8(value.as_bytes())
                        .map_err(|_| "metadata 请求头编码无效")?
                        .to_owned();
                    count += metadata(rule, &mut raw, budget)?;
                    *value = HeaderValue::from_str(&raw).map_err(|_| "metadata 请求头编码无效")?;
                }
            }
            count += metadata_fields(rule, body, changes, budget)?;
            Ok(count)
        }
        PrivacyScope::DesktopGitContext | PrivacyScope::EnvironmentText => {
            contexts.apply(rule, body, changes, budget)
        }
        PrivacyScope::RequestBody => {
            changes.stage(body, "", budget, |next, budget| rule.apply(next, budget))
        }
        PrivacyScope::RequestHeader => {
            let name = reqwest::header::HeaderName::from_bytes(rule.rule.selector.as_bytes())
                .map_err(|_| "请求头名称无效")?;
            let mut items = headers
                .get_all(&name)
                .iter()
                .map(|value| {
                    std::str::from_utf8(value.as_bytes())
                        .map(|s| Value::String(s.to_owned()))
                        .map_err(|_| "目标请求头不是文本")
                })
                .collect::<Result<Vec<_>, _>>()?;
            if items.is_empty() {
                return Ok(0);
            }
            let mut count = 0;
            for index in (0..items.len()).rev() {
                // 复用同一目标执行器，删除根标记只在请求头承载点解释
                let removed = rule.apply_value(&mut items[index], budget, &mut count)?;
                if removed {
                    items.remove(index);
                }
            }
            if count == 0 {
                return Ok(0);
            }
            headers.remove(&name);
            for value in items {
                let value = HeaderValue::from_str(value.as_str().ok_or("请求头只能保存字符串")?)
                    .map_err(|_| "替换结果不能作为请求头")?;
                // WS opening 会忽略不能转为 ASCII 文本的头，预览应提前暴露这个失败
                value
                    .to_str()
                    .map_err(|_| "替换结果必须是 ASCII 请求头值")?;
                headers.append(name.clone(), value);
            }
            Ok(count)
        }
    }
}

fn metadata_fields(
    rule: &CompiledRule,
    body: &Value,
    changes: &mut BodyChanges,
    budget: &mut Budget<'_>,
) -> Result<usize, &'static str> {
    let mut count = 0;
    for prefix in ["", "/client_metadata"] {
        for key in METADATA_KEYS {
            let path = format!("{prefix}/{key}");
            if body.pointer(&path).is_none() {
                continue;
            }
            count += changes.stage(body, &path, budget, |next, budget| match next {
                Value::String(raw) => metadata(rule, raw, budget),
                Value::Object(_) => rule.apply(next, budget),
                _ => Err("metadata 必须是 JSON 对象或编码后的字符串"),
            })?;
        }
    }
    Ok(count)
}

fn metadata(
    rule: &CompiledRule,
    raw: &mut String,
    budget: &mut Budget<'_>,
) -> Result<usize, &'static str> {
    budget.text(raw)?;
    let mut value: Value = serde_json::from_str(raw).map_err(|_| "metadata JSON 无法解析")?;
    if !value.is_object() {
        return Err("metadata 必须是对象");
    }
    let count = rule.apply(&mut value, budget)?;
    if count > 0 {
        let encoded = crate::transport::request::serialize_ascii_turn_metadata(&value)
            .ok_or("metadata 无法编码")?;
        if encoded.len() > super::MAX_VALUE_BYTES {
            return Err("metadata 编码结果超出大小限制");
        }
        *raw = encoded;
    }
    Ok(count)
}

#[derive(Default)]
pub(super) struct ContextIndex {
    desktop: Option<Vec<ContextTarget>>,
    environment: Option<Vec<ContextTarget>>,
}

struct ContextTarget {
    path: String,
    open: &'static str,
    close: &'static str,
    identified: bool,
}

impl ContextIndex {
    fn apply(
        &mut self,
        rule: &CompiledRule,
        body: &Value,
        changes: &mut BodyChanges,
        budget: &mut Budget<'_>,
    ) -> Result<usize, &'static str> {
        let desktop = rule.rule.scope == PrivacyScope::DesktopGitContext;
        let targets = if desktop {
            &mut self.desktop
        } else {
            &mut self.environment
        };
        if targets.is_none() {
            *targets = Some(locate_contexts(body, desktop, budget)?);
        }
        let mut count = 0;
        for target in targets.iter().flatten() {
            budget.step()?;
            // 先检查长度再复制，普通对话和工具输出不会进入事务副本
            let text = body
                .pointer(&target.path)
                .and_then(Value::as_str)
                .ok_or("上下文文本不存在")?;
            let trimmed = text.trim();
            let complete = trimmed.starts_with(target.open) && trimmed.ends_with(target.close);
            if !complete {
                if target.identified {
                    return Err("已识别上下文不完整，可能已被截断");
                }
                continue;
            }
            budget.text(text)?;
            count += changes.stage(body, &target.path, budget, |next, budget| {
                let Value::String(text) = next else {
                    return Err("上下文必须是文本");
                };
                if desktop {
                    git(rule, text, budget)
                } else {
                    environment(rule, text, budget)
                }
            })?;
        }
        Ok(count)
    }
}

fn locate_contexts(
    body: &Value,
    desktop: bool,
    budget: &mut Budget<'_>,
) -> Result<Vec<ContextTarget>, &'static str> {
    let mut targets = Vec::new();
    let Some(items) = body.get("input").and_then(Value::as_array) else {
        return Ok(targets);
    };
    for (item_index, item) in items.iter().enumerate() {
        budget.step()?;
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
            budget.step()?;
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

fn git(
    rule: &CompiledRule,
    text: &mut String,
    budget: &mut Budget<'_>,
) -> Result<usize, &'static str> {
    let document = parse_xml(text)?;
    budget.step()?;
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
    let count = rule.apply(&mut value, budget)?;
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
        if text.len() - (end - start) + escaped.len() > super::MAX_VALUE_BYTES {
            return Err("Desktop Git 编码结果超出大小限制");
        }
        text.replace_range(start..end, &escaped);
    }
    Ok(count)
}

fn environment(
    rule: &CompiledRule,
    text: &mut String,
    budget: &mut Budget<'_>,
) -> Result<usize, &'static str> {
    let before = xml_shape(text, budget)?;
    let mut value = Value::String(text.clone());
    let count = rule.apply(&mut value, budget)?;
    let result = value.as_str().ok_or("环境上下文必须是文本")?;
    if xml_shape(result, budget)? != before {
        return Err("替换不能改变环境上下文包裹结构");
    }
    *text = result.to_owned();
    Ok(count)
}

fn parse_xml(text: &str) -> Result<roxmltree::Document<'_>, &'static str> {
    roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            nodes_limit: 10_000,
            ..Default::default()
        },
    )
    .map_err(|_| "上下文 XML 无法解析或节点数量超限")
}

fn xml_shape(text: &str, budget: &mut Budget<'_>) -> Result<Vec<(String, usize)>, &'static str> {
    let document = parse_xml(text)?;
    let mut shape = Vec::new();
    for node in document.descendants().filter(|node| node.is_element()) {
        budget.step()?;
        let depth = node.ancestors().take(65).count();
        if depth > 64 {
            return Err("上下文 XML 嵌套超过 64 层");
        }
        shape.push((node.tag_name().name().to_owned(), depth));
    }
    Ok(shape)
}
