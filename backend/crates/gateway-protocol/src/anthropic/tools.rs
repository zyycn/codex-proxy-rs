//! 工具名称、custom 参数和原生工具声明的双向映射

use std::collections::BTreeMap;

use super::TranslationError;

use serde_json::{Map, Value, json};

#[derive(Debug, Clone)]
pub(super) struct Tool<D> {
    pub name: String,
    pub namespace: Option<String>,
    pub custom: bool,
    pub definition: D,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ToolSet<D = ()> {
    pub tools: BTreeMap<String, Tool<D>>,
}

impl<D> ToolSet<D> {
    fn collect<'a>(request: &'a Map<String, Value>, definition: impl Fn(&'a Value) -> D) -> Self {
        let mut result = Self {
            tools: BTreeMap::new(),
        };
        for tool in request
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if tool.get("type").and_then(Value::as_str) == Some("namespace") {
                let namespace = tool.get("name").and_then(Value::as_str).unwrap_or_default();
                for child in tool
                    .get("tools")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    result.insert(child, Some(namespace), definition(child));
                }
            } else if !is_web_search(tool) {
                result.insert(tool, None, definition(tool));
            }
        }
        result
    }

    fn insert(&mut self, definition: &Value, namespace: Option<&str>, retained: D) {
        let custom = definition.get("type").and_then(Value::as_str) == Some("custom");
        let Some(name) = definition.get("name").and_then(Value::as_str) else {
            return;
        };
        let qualified = namespace.map_or_else(|| name.to_owned(), |ns| format!("{ns}__{name}"));
        let mut wire_name = qualified;
        if wire_name.len() > 64
            || !wire_name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            wire_name = format!("tool_{}", self.tools.len());
        }
        let mut alias_index = self.tools.len();
        while self.tools.contains_key(&wire_name) {
            wire_name = format!("tool_{alias_index}");
            alias_index += 1;
        }
        self.tools.insert(
            wire_name,
            Tool {
                name: name.to_owned(),
                namespace: namespace.map(str::to_owned),
                custom,
                definition: retained,
            },
        );
    }

    pub fn wire_name<'a>(&'a self, name: &'a str, namespace: Option<&str>) -> &'a str {
        self.tools
            .iter()
            .find_map(|(wire, tool)| {
                (tool.name == name && tool.namespace.as_deref() == namespace)
                    .then_some(wire.as_str())
            })
            .unwrap_or(name)
    }

    pub fn call(&self, block: &Value, item_id: &str) -> Value {
        let wire_name = block
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let tool = self.tools.get(wire_name);
        let name = tool.map_or(wire_name, |tool| tool.name.as_str());
        let call_id = block.get("id").and_then(Value::as_str).unwrap_or_default();
        let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
        let mut item = if tool.is_some_and(|tool| tool.custom) {
            json!({"type":"custom_tool_call","id":item_id,"call_id":call_id,"name":name,"input":input.get("input").and_then(Value::as_str).unwrap_or_default(),"status":"completed"})
        } else {
            json!({"type":"function_call","id":item_id,"call_id":call_id,"name":name,"arguments":input.to_string(),"status":"completed"})
        };
        if let Some(namespace) = tool.and_then(|tool| tool.namespace.as_deref()) {
            item["namespace"] = json!(namespace);
        }
        item
    }
}

impl ToolSet {
    pub fn identities(request: &Map<String, Value>) -> Self {
        Self::collect(request, |_| ())
    }
}

impl<'a> ToolSet<&'a Value> {
    pub fn from_responses(request: &'a Map<String, Value>) -> Self {
        Self::collect(request, |tool| tool)
    }

    fn definitions(&self, allowed: Option<&[Value]>) -> Vec<Value> {
        self.tools.iter().filter(|(_, tool)| {
            allowed.is_none_or(|choices| choices.iter().any(|choice|
                matches!(choice["type"].as_str(), Some("function" | "custom"))
                && choice["type"] == tool.definition["type"]
                && choice.get("name").and_then(Value::as_str) == Some(tool.name.as_str())
                && choice.get("namespace").and_then(Value::as_str) == tool.namespace.as_deref()
            ))
        }).map(|(name, tool)| {
            let schema = if tool.custom {
                json!({"type":"object","properties":{"input":{"type":"string"}},"required":["input"]})
            } else {
                tool.definition.get("parameters").cloned().unwrap_or_else(|| json!({"type":"object","properties":{}}))
            };
            let mut definition = json!({"name":name,"input_schema":schema});
            for field in ["description", "strict", "cache_control", "defer_loading"] {
                if let Some(value) = tool.definition.get(field) { definition[field] = value.clone(); }
            }
            definition
        }).collect()
    }

    pub fn write_messages_tools(
        &self,
        request: &Map<String, Value>,
        target: &mut Map<String, Value>,
    ) -> Result<(), TranslationError> {
        let choice = request.get("tool_choice").unwrap_or(&Value::Null);
        let restricted = choice["type"] == "allowed_tools";
        let allowed = if restricted {
            Some(
                choice["tools"]
                    .as_array()
                    .filter(|v| !v.is_empty())
                    .ok_or(TranslationError::InvalidRequest(
                        "Allowed tools must contain at least one tool.",
                    ))?
                    .as_slice(),
            )
        } else {
            None
        };
        let mut definitions = self.definitions(allowed);
        for tool in request
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|t| is_web_search(t))
        {
            // Messages 的搜索是实时联网；缓存或索引模式不能升级为实时访问
            if tool
                .get("external_web_access")
                .is_some_and(|v| !v.is_null() && v != true && v != "live")
                || allowed.is_some_and(|choices| !choices.iter().any(|c| c["type"] == tool["type"]))
            {
                continue;
            }
            let mut search = json!({"type":"web_search_20250305","name":"web_search"});
            if let Some(filters) = tool.get("filters").filter(|v| !v.is_null()) {
                let filters = filters.as_object().ok_or(TranslationError::InvalidRequest(
                    "Web search filters must be an object.",
                ))?;
                for field in ["allowed_domains", "blocked_domains"] {
                    if let Some(value) = filters.get(field) {
                        search[field] = value.clone();
                    }
                }
            }
            for field in ["max_uses", "user_location"] {
                if let Some(value) = tool.get(field) {
                    search[field] = value.clone();
                }
            }
            definitions.push(search);
        }
        if definitions.is_empty() {
            if restricted {
                return Err(TranslationError::InvalidRequest(
                    "No allowed tool can be used with the Messages upstream.",
                ));
            }
            return Ok(());
        }
        target.insert("tools".into(), Value::Array(definitions));
        let serial = request.get("parallel_tool_calls").and_then(Value::as_bool) == Some(false);
        if choice.is_null() && !serial {
            return Ok(());
        }
        let kind = if restricted {
            match choice["mode"].as_str() {
                Some(mode @ ("auto" | "required")) => mode,
                _ => {
                    return Err(TranslationError::InvalidRequest(
                        "Allowed tools mode must be auto or required.",
                    ));
                }
            }
        } else {
            choice
                .as_str()
                .or_else(|| choice.get("type").and_then(Value::as_str))
                .unwrap_or("auto")
        };
        let mut mapped = match kind {
            "required" => json!({"type":"any"}),
            "none" => json!({"type":"none"}),
            "function" | "custom" => {
                json!({"type":"tool","name":self.wire_name(choice["name"].as_str().unwrap_or_default(),choice["namespace"].as_str())})
            }
            _ => json!({"type":"auto"}),
        };
        if serial && kind != "none" {
            mapped["disable_parallel_tool_use"] = json!(true);
        }
        target.insert("tool_choice".into(), mapped);
        Ok(())
    }
}

fn is_web_search(tool: &Value) -> bool {
    matches!(
        tool["type"].as_str(),
        Some("web_search" | "web_search_preview")
    )
}

pub(super) fn messages_tools(tools: &[Value]) -> Vec<Value> {
    tools.iter().map(|tool| {
        let kind = tool.get("type").and_then(Value::as_str).unwrap_or_default();
        if kind.starts_with("web_search_") {
            let mut search = json!({"type":"web_search"});
            for field in ["allowed_domains", "blocked_domains"] {
                if let Some(v) = tool.get(field) { search["filters"][field] = v.clone(); }
            }
            if let Some(v) = tool.get("user_location") { search["user_location"] = v.clone(); }
            return search;
        }
        let mut result = json!({"type":"function","name":tool.get("name").and_then(Value::as_str).unwrap_or_default(),"parameters":tool.get("input_schema").cloned().unwrap_or_else(||json!({"type":"object","properties":{}}))});
        for field in ["description", "strict"] {
            if let Some(value) = tool.get(field) { result[field] = value.clone(); }
        }
        result
    }).collect()
}

// 截断的 custom 包装仍可携带已生成的字符串，未完成的转义按原片段保留
pub(super) fn partial_custom_input(arguments: &str) -> String {
    let Some((_, rest)) = arguments.split_once("\"input\"") else {
        return arguments.to_owned();
    };
    let Some(value) = rest.trim_start().strip_prefix(':').map(str::trim_start) else {
        return arguments.to_owned();
    };
    if let Some(Ok(input)) = serde_json::Deserializer::from_str(value)
        .into_iter::<String>()
        .next()
    {
        return input;
    }
    // 一个未完成的 UTF-16 代理对最多占 12 字节，只尝试修复字符串尾部
    for end in std::iter::once(value.len())
        .chain(value.char_indices().rev().take(12).map(|(index, _)| index))
    {
        if let Ok(mut input) = serde_json::from_str::<String>(&format!("{}\"", &value[..end])) {
            input.push_str(&value[end..]);
            return input;
        }
    }
    arguments.to_owned()
}
