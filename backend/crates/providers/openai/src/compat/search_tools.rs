//! 识别普通与 Responses Lite 请求中的托管搜索声明，不改写工具配置

use serde_json::{Map, Value};

pub(crate) fn declares_hosted_search(body: &Map<String, Value>) -> bool {
    body.get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| tools_declare_search(tools))
        || body
            .get("input")
            .and_then(Value::as_array)
            .is_some_and(|input| {
                input.iter().any(|item| {
                    matches!(
                        item.get("type").and_then(Value::as_str),
                        Some("additional_tools" | "tool_search_output")
                    ) && item
                        .get("tools")
                        .and_then(Value::as_array)
                        .is_some_and(|tools| tools_declare_search(tools))
                })
            })
}

pub(super) fn tools_declare_search(tools: &[Value]) -> bool {
    tools.iter().any(|tool| {
        tool.get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == "web_search" || kind.starts_with("web_search_preview"))
    })
}
