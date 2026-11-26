//! Responses 内置搜索声明与强制工具选择，共用同一协议判定

use serde_json::{Map, Value};

/// 强制搜索无法对应唯一声明，不替调用方猜测搜索参数
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSelectionError {
    MissingDeclaration,
    MultipleDeclarations,
}

impl SearchSelectionError {
    /// 只包含静态协议诊断，可交给请求边界展示
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::MissingDeclaration => "The selected web search tool must be declared.",
            Self::MultipleDeclarations => "Select one web search declaration.",
        }
    }
}

impl std::fmt::Display for SearchSelectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for SearchSelectionError {}

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

/// 只收窄搜索声明的允许范围，不把工具选择中的引用当成完整声明
pub fn allowed_declarations(body: &Map<String, Value>) -> impl Iterator<Item = &Value> {
    let choice = body.get("tool_choice").unwrap_or(&Value::Null);
    body.get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(move |tool| {
            is_search(tool)
                && (choice["type"] != "allowed_tools"
                    || choice["tools"].as_array().is_some_and(|allowed| {
                        allowed
                            .iter()
                            .any(|reference| reference["type"] == tool["type"])
                    }))
        })
}

/// 可选搜索返回 None；强制搜索必须选出唯一声明，缺失或歧义不能退回普通生成
pub fn required_tool(body: &Map<String, Value>) -> Result<Option<&Value>, SearchSelectionError> {
    if !is_required(body) {
        return Ok(None);
    }
    let choice = body.get("tool_choice").unwrap_or(&Value::Null);
    let mut tools = allowed_declarations(body)
        .filter(|tool| !is_search(choice) || choice["type"] == tool["type"]);
    let tool = tools
        .next()
        .ok_or(SearchSelectionError::MissingDeclaration)?;
    if tools.next().is_some() {
        return Err(SearchSelectionError::MultipleDeclarations);
    }
    Ok(Some(tool))
}

/// 未限制联网或明确选择 live 时允许实时访问；目标上游决定能否兑现其他模式
#[must_use]
pub fn allows_live_access(tool: &Value) -> bool {
    tool.get("external_web_access")
        .is_none_or(|value| value.is_null() || value == true || value == "live")
}
