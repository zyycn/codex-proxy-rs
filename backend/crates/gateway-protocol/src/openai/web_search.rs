//! Responses 内置搜索声明与强制工具选择，共用同一协议判定

use serde_json::{Map, Value};

#[must_use]
pub fn is_search(tool: &Value) -> bool {
    matches!(
        tool["type"].as_str(),
        Some(
            "web_search"
                | "web_search_preview"
                | "web_search_2025_08_26"
                | "web_search_preview_2025_03_11"
        )
    )
}

/// 只有客户端的选择要求本轮执行搜索时才可以派发独立搜索请求
#[must_use]
pub fn is_required(body: &Map<String, Value>) -> bool {
    let Some(choice) = body.get("tool_choice") else {
        return false;
    };
    if is_search(choice) {
        return true;
    }
    let tools = if choice["type"] == "allowed_tools" && choice["mode"] == "required" {
        choice["tools"].as_array()
    } else if choice == "required" {
        body.get("tools").and_then(Value::as_array)
    } else {
        None
    };
    tools.is_some_and(|tools| !tools.is_empty() && tools.iter().all(is_search))
}
