//! Codex/OpenAI 工具声明与历史调用到 Grok 工具合同的归一化。

use super::response::{ToolIdentity, ToolKind};
use super::*;

pub(super) struct ToolNormalizer {
    pub(super) response: GrokResponseTransform,
    identity_aliases: BTreeMap<ToolIdentity, String>,
    deferred_surfaces: Vec<String>,
    client_search_tool: Option<Map<String, Value>>,
    server_search_eager: bool,
    native_shell: bool,
}

pub(super) struct NormalizedInputItems {
    items: Vec<Value>,
    loaded_tools: Vec<Value>,
    visible_tools: Vec<Value>,
}

pub(super) struct NormalizedToolSearchOutput {
    history: Map<String, Value>,
    loaded_tools: Vec<Value>,
    visible_tools: Vec<Value>,
}

impl ToolNormalizer {
    pub(super) fn new() -> Self {
        Self {
            response: GrokResponseTransform::default(),
            identity_aliases: BTreeMap::new(),
            deferred_surfaces: Vec::new(),
            client_search_tool: None,
            server_search_eager: false,
            native_shell: false,
        }
    }

    pub(super) fn for_replay(response: GrokResponseTransform) -> Self {
        let identity_aliases = response
            .aliases
            .iter()
            .map(|(alias, identity)| (identity.clone(), alias.clone()))
            .collect();
        Self {
            response,
            identity_aliases,
            deferred_surfaces: Vec::new(),
            client_search_tool: None,
            server_search_eager: false,
            native_shell: false,
        }
    }

    pub(super) fn normalize(
        mut self,
        payload: &mut Map<String, Value>,
    ) -> Result<GrokResponseTransform, GrokRequestEncodeError> {
        let (tools, had_tools) =
            optional_array(payload.get("tools")).map_err(|error| error.at_field("tools"))?;
        if had_tools {
            self.response.visible_tools.clone_from(&tools);
        }
        let client_search = inspect_tool_search(&tools).map_err(|error| error.at_field("tools"))?;
        self.normalize_client_search_parallel(payload, client_search)
            .map_err(|error| error.at_field("parallel_tool_calls"))?;

        let mut normalized_tools = Vec::with_capacity(tools.len());
        for raw_tool in &tools {
            normalized_tools.extend(
                self.normalize_tool(raw_tool, "", client_search, false)
                    .map_err(|error| error.at_field("tools"))?,
            );
        }

        if let Some(Value::Array(items)) = payload.get("input") {
            let normalized = self
                .normalize_input_items(items)
                .map_err(|error| error.at_field("input"))?;
            normalized_tools.extend(normalized.loaded_tools);
            self.response.visible_tools.extend(normalized.visible_tools);
            payload.insert("input".to_owned(), Value::Array(normalized.items));
        } else if payload
            .get("input")
            .is_some_and(|input| !input.is_null() && !input.is_string())
        {
            return Err(GrokRequestEncodeError::InvalidRequestField { field: "input" });
        }

        if self.client_search_tool.is_some() {
            normalized_tools.push(Value::Object(
                self.build_client_search_function()
                    .map_err(|error| error.at_field("tools"))?,
            ));
        }
        normalized_tools = dedupe_normalized_tools(normalized_tools);
        if normalized_tools.is_empty() {
            if had_tools {
                payload.remove("tools");
                payload.remove("parallel_tool_calls");
            }
        } else {
            payload.insert("tools".to_owned(), Value::Array(normalized_tools.clone()));
        }
        self.normalize_tool_choice(payload, &normalized_tools)
            .map_err(|error| error.at_field("tool_choice"))?;
        Ok(self.response)
    }

    fn alias(&mut self, identity: ToolIdentity) -> String {
        if let Some(alias) = self.identity_aliases.get(&identity) {
            return alias.clone();
        }
        let base = match identity.kind {
            ToolKind::ToolSearch => "xai_proxy_tool_search".to_owned(),
            ToolKind::ApplyPatch => "xai_proxy_apply_patch".to_owned(),
            ToolKind::Function | ToolKind::Custom if !identity.namespace.is_empty() => {
                format!("{}__{}", identity.namespace, identity.name)
            }
            ToolKind::Function | ToolKind::Custom => identity.name.clone(),
        };
        let key = format!(
            "{}\0{}\0{}",
            identity.kind as u8, identity.namespace, identity.name
        );
        let mut alias = truncate_tool_alias(&base, &key);
        if self
            .response
            .aliases
            .get(&alias)
            .is_some_and(|existing| existing != &identity)
        {
            alias = hashed_tool_alias(&base, &key);
        }
        self.response
            .aliases
            .insert(alias.clone(), identity.clone());
        self.identity_aliases.insert(identity, alias.clone());
        alias
    }

    fn normalize_client_search_parallel(
        &self,
        payload: &mut Map<String, Value>,
        client_search: bool,
    ) -> Result<(), GrokRequestEncodeError> {
        if !client_search {
            return Ok(());
        }
        match payload.get("parallel_tool_calls") {
            None | Some(Value::Null) => {
                payload.insert("parallel_tool_calls".to_owned(), Value::Bool(false));
            }
            Some(Value::Bool(true)) => {
                payload.insert("parallel_tool_calls".to_owned(), Value::Bool(false));
            }
            Some(Value::Bool(false)) => {}
            Some(_) => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
        }
        Ok(())
    }

    fn normalize_tool(
        &mut self,
        raw: &Value,
        namespace: &str,
        client_search: bool,
        force: bool,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        let tool = raw
            .as_object()
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        let kind = string_field(tool, "type").trim();
        match kind {
            "function" => self.normalize_function_tool(tool, namespace, client_search, force),
            "namespace" => self.normalize_namespace_tool(tool, client_search, force),
            "tool_search" => self.normalize_tool_search(tool, force),
            "custom" => self.normalize_custom_tool(tool, namespace),
            "web_search"
            | "web_search_preview"
            | "web_search_preview_2025_03_11"
            | "web_search_2025_08_26" => self.normalize_web_search_tool(tool, kind),
            "mcp" => self.normalize_mcp_tool(tool, client_search, force),
            "shell" => self.normalize_shell_tool(tool),
            "local_shell" => self.normalize_legacy_local_shell_tool(tool),
            "apply_patch" => self.normalize_apply_patch_tool(tool),
            "x_search" | "collections_search" | "file_search" | "code_execution"
            | "code_interpreter" => Ok(vec![Value::Object(without_defer_loading(tool))]),
            // 对未获 xAI 接受的工具做静默过滤，并在过滤后同步收敛 tool_choice；
            // 不要把客户端可选扩展升级成整单 400。
            "" | "computer_use_preview" | "image_generation" => Ok(Vec::new()),
            _ => Ok(Vec::new()),
        }
    }

    fn normalize_function_tool(
        &mut self,
        tool: &Map<String, Value>,
        namespace: &str,
        client_search: bool,
        force: bool,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        let name = string_field(tool, "name").trim();
        if name.is_empty() {
            return Err(GrokRequestEncodeError::InvalidRequestNormalization);
        }
        let deferred = tool
            .get("defer_loading")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if deferred && client_search && !force {
            if namespace.is_empty() {
                self.deferred_surfaces.push(describe_deferred_tool(
                    name,
                    string_field(tool, "description"),
                ));
            }
            return Ok(Vec::new());
        }
        let mut converted = without_defer_loading(tool);
        let function_schema = tool
            .get("parameters")
            .filter(|schema| schema_contains_integer(schema, 0))
            .cloned();
        match converted.get("parameters").cloned() {
            None | Some(Value::Null) => {
                converted.insert(
                    "parameters".to_owned(),
                    json_object([
                        ("type", Value::String("object".to_owned())),
                        ("properties", Value::Object(Map::new())),
                    ]),
                );
            }
            Some(parameters) => {
                if let Some(normalized) = normalize_function_parameters_root(&parameters)? {
                    converted.insert("parameters".to_owned(), normalized);
                }
            }
        }
        let alias = self.alias(ToolIdentity::new(ToolKind::Function, namespace, name));
        if let Some(schema) = function_schema {
            self.response.function_schemas.insert(alias.clone(), schema);
        }
        converted.insert("name".to_owned(), Value::String(alias));
        Ok(vec![Value::Object(converted)])
    }

    fn normalize_namespace_tool(
        &mut self,
        tool: &Map<String, Value>,
        client_search: bool,
        force: bool,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        let name = string_field(tool, "name").trim();
        let children = tool
            .get("tools")
            .and_then(Value::as_array)
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        if name.is_empty() {
            return Err(GrokRequestEncodeError::InvalidRequestNormalization);
        }
        if client_search && !force && namespace_has_deferred_functions(children) {
            self.deferred_surfaces.push(describe_deferred_tool(
                name,
                string_field(tool, "description"),
            ));
        }
        let mut converted = Vec::new();
        for child in children {
            if child.pointer("/type").and_then(Value::as_str) != Some("function") {
                return Err(GrokRequestEncodeError::InvalidRequestNormalization);
            }
            converted.extend(self.normalize_tool(child, name, client_search, force)?);
        }
        Ok(converted)
    }

    fn normalize_tool_search(
        &mut self,
        tool: &Map<String, Value>,
        force: bool,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        if force {
            return Ok(Vec::new());
        }
        match string_field(tool, "execution")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "" | "server" => {
                self.server_search_eager = true;
            }
            "client" => {
                self.client_search_tool = Some(tool.clone());
            }
            _ => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
        }
        Ok(Vec::new())
    }

    fn normalize_custom_tool(
        &mut self,
        tool: &Map<String, Value>,
        namespace: &str,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        let name = string_field(tool, "name").trim();
        if name.is_empty() || tool.get("format").is_some_and(|value| !value.is_object()) {
            return Err(GrokRequestEncodeError::InvalidRequestNormalization);
        }
        let identity = ToolIdentity::new(ToolKind::Custom, namespace, name);
        let argument_field = identity.custom_argument_field();
        let description = if identity.is_root_custom_apply_patch() {
            "The apply_patch tool edits files using Codex patch format. Provide the complete raw patch text in the patch string field.".to_owned()
        } else {
            let mut description = string_field(tool, "description").trim().to_owned();
            if !description.is_empty() {
                description.push('\n');
            }
            description.push_str("Provide the custom tool input in the input string field.");
            description
        };
        let alias = self.alias(identity);
        Ok(vec![json_object([
            ("type", Value::String("function".to_owned())),
            ("name", Value::String(alias)),
            ("description", Value::String(description)),
            (
                "parameters",
                json_object([
                    ("type", Value::String("object".to_owned())),
                    (
                        "properties",
                        json_object([(
                            argument_field,
                            json_object([("type", Value::String("string".to_owned()))]),
                        )]),
                    ),
                    (
                        "required",
                        Value::Array(vec![Value::String(argument_field.to_owned())]),
                    ),
                    ("additionalProperties", Value::Bool(false)),
                ]),
            ),
        ])])
    }

    fn build_client_search_function(
        &mut self,
    ) -> Result<Map<String, Value>, GrokRequestEncodeError> {
        let tool = self
            .client_search_tool
            .as_ref()
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        let mut description = string_field(tool, "description").trim().to_owned();
        if description.is_empty() {
            description.push_str("Search for tools needed to continue the task.");
        }
        if !self.deferred_surfaces.is_empty() {
            description.push_str("\nDeferred tool surfaces available to search:\n- ");
            description.push_str(&self.deferred_surfaces.join("\n- "));
        }
        description.truncate(description.len().min(MAX_TOOL_SEARCH_DESCRIPTION_BYTES));
        let parameters = match tool.get("parameters") {
            None => json_object([
                ("type", Value::String("object".to_owned())),
                ("properties", Value::Object(Map::new())),
                ("additionalProperties", Value::Bool(true)),
            ]),
            Some(Value::Object(parameters)) => Value::Object(parameters.clone()),
            Some(_) => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
        };
        let alias = self.alias(ToolIdentity::new(ToolKind::ToolSearch, "", "tool_search"));
        Ok(Map::from_iter([
            ("type".to_owned(), Value::String("function".to_owned())),
            ("name".to_owned(), Value::String(alias)),
            ("description".to_owned(), Value::String(description)),
            ("parameters".to_owned(), parameters),
        ]))
    }
}

pub(super) fn optional_array(
    value: Option<&Value>,
) -> Result<(Vec<Value>, bool), GrokRequestEncodeError> {
    match value {
        None | Some(Value::Null) => Ok((Vec::new(), false)),
        Some(Value::Array(values)) => Ok((values.clone(), true)),
        Some(_) => Err(GrokRequestEncodeError::InvalidRequestNormalization),
    }
}

/// Grok Build 要求 function parameters 的根节点必为 object；Codex 会为可选参数
/// 生成 `object | null`，这里只移除根节点 nullability，嵌套字段保持原样。
pub(super) fn normalize_function_parameters_root(
    value: &Value,
) -> Result<Option<Value>, GrokRequestEncodeError> {
    let Some(schema) = value.as_object() else {
        return Ok(None);
    };
    let mut normalized = schema.clone();
    let mut changed = false;

    if let Some(types) = normalized.get("type").and_then(Value::as_array).cloned() {
        let removed_null = types.iter().any(|value| value.as_str() == Some("null"));
        if removed_null {
            let remaining = types
                .into_iter()
                .filter(|value| value.as_str() != Some("null"))
                .collect::<Vec<_>>();
            if remaining.len() != 1 || remaining[0].as_str() != Some("object") {
                return Err(invalid_function_parameters_root());
            }
            normalized.insert("type".to_owned(), Value::String("object".to_owned()));
            changed = true;
        }
    }

    for keyword in ["anyOf", "oneOf"] {
        let Some(branches) = normalized.get(keyword).and_then(Value::as_array).cloned() else {
            continue;
        };
        let removed_null = branches.iter().any(is_null_only_schema);
        if !removed_null {
            continue;
        }
        let remaining = branches
            .into_iter()
            .filter(|branch| !is_null_only_schema(branch))
            .collect::<Vec<_>>();
        if remaining.is_empty()
            || remaining.iter().any(|branch| {
                branch.as_object().is_none_or(|branch| {
                    !is_object_root_schema(branch, &normalized, &mut BTreeSet::new())
                })
            })
        {
            return Err(invalid_function_parameters_root());
        }
        if remaining.len() == 1 && normalized.len() == 1 {
            normalized = remaining[0]
                .as_object()
                .cloned()
                .ok_or_else(invalid_function_parameters_root)?;
            normalized.insert("type".to_owned(), Value::String("object".to_owned()));
        } else {
            normalized.insert(keyword.to_owned(), Value::Array(remaining));
            normalized.insert("type".to_owned(), Value::String("object".to_owned()));
        }
        changed = true;
    }

    Ok(changed.then_some(Value::Object(normalized)))
}

pub(super) fn is_null_only_schema(value: &Value) -> bool {
    let Some(schema) = value.as_object() else {
        return false;
    };
    match schema.get("type") {
        Some(Value::String(kind)) => kind == "null",
        Some(Value::Array(types)) if !types.is_empty() => {
            types.iter().all(|kind| kind.as_str() == Some("null"))
        }
        _ => false,
    }
}

pub(super) fn is_object_root_schema(
    schema: &Map<String, Value>,
    root: &Map<String, Value>,
    visited: &mut BTreeSet<String>,
) -> bool {
    match schema.get("type") {
        Some(Value::String(kind)) => return kind == "object",
        Some(Value::Array(types)) if !types.is_empty() => {
            return types.iter().all(|kind| kind.as_str() == Some("object"));
        }
        Some(_) => return false,
        None => {}
    }
    if schema.contains_key("properties") {
        return true;
    }
    let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
        return false;
    };
    if !visited.insert(reference.to_owned()) {
        return false;
    }
    resolve_local_schema_ref(root, reference)
        .is_some_and(|resolved| is_object_root_schema(resolved, root, visited))
}

pub(super) fn resolve_local_schema_ref<'a>(
    root: &'a Map<String, Value>,
    reference: &str,
) -> Option<&'a Map<String, Value>> {
    if reference == "#" {
        return Some(root);
    }
    let path = reference.strip_prefix("#/")?;
    let mut segments = path.split('/');
    let first = decode_json_pointer_segment(segments.next()?);
    let mut current = root.get(&first)?;
    for segment in segments {
        let segment = decode_json_pointer_segment(segment);
        current = current.as_object()?.get(&segment)?;
    }
    current.as_object()
}

pub(super) fn decode_json_pointer_segment(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}

pub(super) const fn invalid_function_parameters_root() -> GrokRequestEncodeError {
    GrokRequestEncodeError::InvalidRequestField {
        field: "tools[].parameters",
    }
}

pub(super) fn inspect_tool_search(tools: &[Value]) -> Result<bool, GrokRequestEncodeError> {
    let mut client_search = false;
    let mut server_search = false;
    for raw_tool in tools {
        let tool = raw_tool
            .as_object()
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        if string_field(tool, "type") != "tool_search" {
            continue;
        }
        match string_field(tool, "execution")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "" | "server" if !client_search => server_search = true,
            "client" if !client_search && !server_search => client_search = true,
            _ => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
        }
    }
    Ok(client_search)
}

pub(super) fn string_field<'a>(value: &'a Map<String, Value>, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

pub(super) fn json_object<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Object(Map::from_iter(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value)),
    ))
}

pub(super) fn without_defer_loading(tool: &Map<String, Value>) -> Map<String, Value> {
    let mut converted = tool.clone();
    converted.remove("defer_loading");
    converted
}

pub(super) fn namespace_has_deferred_functions(children: &[Value]) -> bool {
    children.iter().any(|child| {
        child.pointer("/type").and_then(Value::as_str) == Some("function")
            && child
                .pointer("/defer_loading")
                .and_then(Value::as_bool)
                .unwrap_or(false)
    })
}

pub(super) fn describe_deferred_tool(name: &str, description: &str) -> String {
    let description = description.trim();
    if description.is_empty() {
        return name.to_owned();
    }
    let description = description.chars().take(240).collect::<String>();
    format!("{name}: {description}")
}

pub(super) fn truncate_tool_alias(base: &str, key: &str) -> String {
    if base.len() <= MAX_BUILD_TOOL_ALIAS_LENGTH {
        base.to_owned()
    } else {
        hashed_tool_alias(base, key)
    }
}

pub(super) fn hashed_tool_alias(base: &str, key: &str) -> String {
    let suffix = format!("__{}", short_tool_hash(key));
    let limit = MAX_BUILD_TOOL_ALIAS_LENGTH.saturating_sub(suffix.len());
    let mut end = limit.min(base.len());
    while !base.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}{suffix}", &base[..end])
}

pub(super) fn short_tool_hash(value: &str) -> String {
    Sha256::digest(value)
        .iter()
        .take(5)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..9]
        .to_owned()
}

pub(super) fn dedupe_normalized_tools(tools: Vec<Value>) -> Vec<Value> {
    let mut result = Vec::with_capacity(tools.len());
    let mut seen = BTreeSet::new();
    for tool in tools {
        if normalized_tool_dedupe_key(&tool).is_none_or(|key| seen.insert(key)) {
            result.push(tool);
        }
    }
    result
}

pub(super) fn normalized_tool_dedupe_key(tool: &Value) -> Option<String> {
    let object = tool.as_object()?;
    let kind = string_field(object, "type").trim();
    if !kind.is_empty() {
        if let Some(name) = object
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            return Some(format!("type:{kind}\0name:{name}"));
        }
        if kind == "mcp"
            && let Some(label) = object
                .get("server_label")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|label| !label.is_empty())
        {
            return Some(format!("type:mcp\0server_label:{label}"));
        }
    }
    serde_json::to_string(tool)
        .ok()
        .map(|encoded| format!("json:{encoded}"))
}

impl ToolNormalizer {
    fn normalize_web_search_tool(
        &mut self,
        tool: &Map<String, Value>,
        kind: &str,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        let filters = normalize_web_search_filters(tool)?;
        if let Some(content_types) = tool.get("search_content_types") {
            content_types
                .as_array()
                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        }
        if kind == "web_search" && tool.len() == 1 {
            return Ok(vec![Value::Object(tool.clone())]);
        }
        let mut converted =
            Map::from_iter([("type".to_owned(), Value::String("web_search".to_owned()))]);
        if let Some(filters) = filters {
            converted.insert("filters".to_owned(), Value::Object(filters));
        }
        Ok(vec![Value::Object(converted)])
    }

    fn normalize_mcp_tool(
        &mut self,
        tool: &Map<String, Value>,
        client_search: bool,
        force: bool,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        let deferred = tool
            .get("defer_loading")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if deferred && client_search && !force {
            let label = ["server_label", "name"]
                .into_iter()
                .find_map(|field| tool.get(field).and_then(Value::as_str))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
            self.deferred_surfaces.push(describe_deferred_tool(
                label,
                string_field(tool, "description"),
            ));
            return Ok(Vec::new());
        }
        Ok(vec![Value::Object(without_defer_loading(tool))])
    }

    fn normalize_shell_tool(
        &mut self,
        tool: &Map<String, Value>,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        if self.response.legacy_local_shell {
            return Err(GrokRequestEncodeError::InvalidRequestNormalization);
        }
        self.native_shell = true;
        Ok(vec![Value::Object(without_defer_loading(tool))])
    }

    fn normalize_legacy_local_shell_tool(
        &mut self,
        _tool: &Map<String, Value>,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        if self.native_shell || self.response.legacy_local_shell {
            return Err(GrokRequestEncodeError::InvalidRequestNormalization);
        }
        self.response.legacy_local_shell = true;
        Ok(vec![json_object([
            ("type", Value::String("shell".to_owned())),
            (
                "environment",
                json_object([("type", Value::String("local".to_owned()))]),
            ),
        ])])
    }

    fn normalize_apply_patch_tool(
        &mut self,
        _tool: &Map<String, Value>,
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        let alias = self.alias(ToolIdentity::new(ToolKind::ApplyPatch, "", "apply_patch"));
        let operation = json_object([
            ("type", Value::String("object".to_owned())),
            (
                "properties",
                json_object([
                    (
                        "type",
                        json_object([
                            ("type", Value::String("string".to_owned())),
                            (
                                "enum",
                                Value::Array(
                                    ["create_file", "update_file", "delete_file"]
                                        .into_iter()
                                        .map(|value| Value::String(value.to_owned()))
                                        .collect(),
                                ),
                            ),
                        ]),
                    ),
                    (
                        "path",
                        json_object([
                            ("type", Value::String("string".to_owned())),
                            ("minLength", Value::from(1)),
                        ]),
                    ),
                    (
                        "diff",
                        json_object([("type", Value::String("string".to_owned()))]),
                    ),
                ]),
            ),
            (
                "required",
                Value::Array(
                    ["type", "path"]
                        .into_iter()
                        .map(|value| Value::String(value.to_owned()))
                        .collect(),
                ),
            ),
            ("additionalProperties", Value::Bool(false)),
        ]);
        Ok(vec![json_object([
            ("type", Value::String("function".to_owned())),
            ("name", Value::String(alias)),
            (
                "description",
                Value::String(
                    "Create, update, or delete one file using a structured V4A patch operation. create_file and update_file require path and diff; delete_file requires path."
                        .to_owned(),
                ),
            ),
            (
                "parameters",
                json_object([
                    ("type", Value::String("object".to_owned())),
                    ("properties", json_object([("operation", operation)])),
                    (
                        "required",
                        Value::Array(vec![Value::String("operation".to_owned())]),
                    ),
                    ("additionalProperties", Value::Bool(false)),
                ]),
            ),
            ("strict", Value::Bool(true)),
        ])])
    }

    fn normalize_tool_choice(
        &mut self,
        payload: &mut Map<String, Value>,
        normalized_tools: &[Value],
    ) -> Result<(), GrokRequestEncodeError> {
        let Some(choice) = payload.get("tool_choice").cloned() else {
            return Ok(());
        };
        if choice.is_null() {
            return Ok(());
        }
        if normalized_tools.is_empty() {
            payload.remove("tool_choice");
            return Ok(());
        }
        let Some(mut object) = choice.as_object().cloned() else {
            return choice
                .as_str()
                .filter(|value| matches!(*value, "none" | "auto" | "required"))
                .map(|_| ())
                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization);
        };
        let kind = string_field(&object, "type").to_owned();
        match kind.as_str() {
            "tool_search" => {
                if self.client_search_tool.is_none() {
                    if self.server_search_eager {
                        payload.insert("tool_choice".to_owned(), Value::String("auto".to_owned()));
                        return Ok(());
                    }
                    return Err(GrokRequestEncodeError::InvalidRequestNormalization);
                }
                let alias = self.alias(ToolIdentity::new(ToolKind::ToolSearch, "", "tool_search"));
                payload.insert(
                    "tool_choice".to_owned(),
                    json_object([
                        ("type", Value::String("function".to_owned())),
                        ("name", Value::String(alias)),
                    ]),
                );
            }
            "custom" => {
                let identity = ToolIdentity::new(
                    ToolKind::Custom,
                    string_field(&object, "namespace").trim(),
                    string_field(&object, "name").trim(),
                );
                let alias = self
                    .identity_aliases
                    .get(&identity)
                    .cloned()
                    .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
                object.insert("type".to_owned(), Value::String("function".to_owned()));
                object.insert("name".to_owned(), Value::String(alias));
                object.remove("namespace");
                payload.insert("tool_choice".to_owned(), Value::Object(object));
            }
            "apply_patch" => {
                let identity = ToolIdentity::new(ToolKind::ApplyPatch, "", "apply_patch");
                let alias = self
                    .identity_aliases
                    .get(&identity)
                    .cloned()
                    .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
                payload.insert(
                    "tool_choice".to_owned(),
                    json_object([
                        ("type", Value::String("function".to_owned())),
                        ("name", Value::String(alias)),
                    ]),
                );
            }
            "allowed_tools" => {
                payload.remove("tool_choice");
            }
            "function" => {
                self.normalize_function_tool_choice(payload, object, normalized_tools)?;
            }
            _ => {
                if let Some(hosted_kind) = normalize_hosted_tool_choice_kind(&kind) {
                    let matching = tools_of_type(normalized_tools, hosted_kind);
                    if matching.is_empty() {
                        payload.remove("tool_choice");
                        return Ok(());
                    }
                    object.insert("type".to_owned(), Value::String(hosted_kind.to_owned()));
                    payload.insert("tool_choice".to_owned(), Value::Object(object));
                } else {
                    payload.remove("tool_choice");
                }
            }
        }
        Ok(())
    }

    fn normalize_function_tool_choice(
        &self,
        payload: &mut Map<String, Value>,
        mut object: Map<String, Value>,
        normalized_tools: &[Value],
    ) -> Result<(), GrokRequestEncodeError> {
        if let Some(Value::Object(function)) = object.get_mut("function") {
            rewrite_namespace_choice(function, &self.identity_aliases)?;
            let name = string_field(function, "name").trim();
            if !name.is_empty() && !has_named_tool(normalized_tools, "function", name) {
                payload.remove("tool_choice");
                return Ok(());
            }
            payload.insert("tool_choice".to_owned(), Value::Object(object));
            return Ok(());
        }
        rewrite_namespace_choice(&mut object, &self.identity_aliases)?;
        let name = string_field(&object, "name").trim();
        if !name.is_empty() && !has_named_tool(normalized_tools, "function", name) {
            payload.remove("tool_choice");
            return Ok(());
        }
        payload.insert("tool_choice".to_owned(), Value::Object(object));
        Ok(())
    }
}

pub(super) fn normalize_web_search_filters(
    tool: &Map<String, Value>,
) -> Result<Option<Map<String, Value>>, GrokRequestEncodeError> {
    let nested = match tool.get("filters") {
        None | Some(Value::Null) => None,
        Some(Value::Object(filters)) => filters
            .get("allowed_domains")
            .map(normalize_allowed_domains)
            .transpose()?,
        Some(_) => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
    };
    let top_level = tool
        .get("allowed_domains")
        .map(normalize_allowed_domains)
        .transpose()?;
    if nested.is_some() && top_level.is_some() && nested != top_level {
        return Err(GrokRequestEncodeError::InvalidRequestNormalization);
    }
    Ok(nested
        .or(top_level)
        .map(|domains| Map::from_iter([("allowed_domains".to_owned(), Value::Array(domains))])))
}

pub(super) fn normalize_allowed_domains(
    value: &Value,
) -> Result<Vec<Value>, GrokRequestEncodeError> {
    let domains = value
        .as_array()
        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
    if domains
        .iter()
        .any(|domain| domain.as_str().map(str::trim).is_none_or(str::is_empty))
    {
        return Err(GrokRequestEncodeError::InvalidRequestNormalization);
    }
    Ok(domains.clone())
}

pub(super) fn normalize_hosted_tool_choice_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "web_search"
        | "web_search_preview"
        | "web_search_preview_2025_03_11"
        | "web_search_2025_08_26" => Some("web_search"),
        "x_search" => Some("x_search"),
        "image_generation" => Some("image_generation"),
        "collections_search" => Some("collections_search"),
        "file_search" => Some("file_search"),
        "code_execution" => Some("code_execution"),
        "code_interpreter" => Some("code_interpreter"),
        "mcp" => Some("mcp"),
        "shell" | "local_shell" => Some("shell"),
        _ => None,
    }
}

pub(super) fn has_tool_type(tools: &[Value], kind: &str) -> bool {
    tools
        .iter()
        .any(|tool| tool.pointer("/type").and_then(Value::as_str) == Some(kind))
}

pub(super) fn has_named_tool(tools: &[Value], kind: &str, name: &str) -> bool {
    tools.iter().any(|tool| {
        tool.pointer("/type").and_then(Value::as_str) == Some(kind)
            && tool.pointer("/name").and_then(Value::as_str) == Some(name)
    })
}

pub(super) fn tools_of_type(tools: &[Value], kind: &str) -> Vec<Value> {
    tools
        .iter()
        .filter(|tool| tool.pointer("/type").and_then(Value::as_str) == Some(kind))
        .cloned()
        .collect()
}

pub(super) fn rewrite_namespace_choice(
    object: &mut Map<String, Value>,
    aliases: &BTreeMap<ToolIdentity, String>,
) -> Result<(), GrokRequestEncodeError> {
    let name = string_field(object, "name").trim();
    let namespace = string_field(object, "namespace").trim();
    if name.is_empty() || namespace.is_empty() {
        return Ok(());
    }
    let identity = ToolIdentity::new(ToolKind::Function, namespace, name);
    let alias = aliases
        .get(&identity)
        .cloned()
        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
    object.insert("name".to_owned(), Value::String(alias));
    object.remove("namespace");
    Ok(())
}

impl ToolNormalizer {
    fn normalize_input_items(
        &mut self,
        items: &[Value],
    ) -> Result<NormalizedInputItems, GrokRequestEncodeError> {
        let mut rewritten = Vec::with_capacity(items.len());
        let mut loaded_tools = Vec::new();
        let mut visible_tools = Vec::new();
        for raw_item in items {
            let Some(item) = raw_item.as_object() else {
                rewritten.push(raw_item.clone());
                continue;
            };
            let mut item_type = string_field(item, "type").trim();
            if item_type.is_empty() && !string_field(item, "role").trim().is_empty() {
                item_type = "message";
            }
            match item_type {
                "message" => rewritten.push(Value::Object(self.normalize_message_input(item)?)),
                "function_call" => {
                    rewritten.push(Value::Object(self.normalize_function_call_input(item)?));
                }
                "function_call_output" => rewritten.push(Value::Object(
                    self.normalize_function_call_output_input(item, true)?,
                )),
                "reasoning" => rewritten.push(Value::Object(sanitize_reasoning_input(item))),
                "file_search_call"
                | "web_search_call"
                | "image_generation_call"
                | "code_interpreter_call"
                | "shell_call"
                | "mcp_list_tools"
                | "mcp_approval_request"
                | "mcp_approval_response"
                | "mcp_call" => {
                    rewritten.push(Value::Object(sanitize_native_history_input(
                        item, item_type,
                    )));
                }
                "compaction" | "compaction_summary" => {
                    rewritten.extend(normalize_compaction_input(item)?);
                }
                "tool_search_call" => {
                    rewritten.push(Value::Object(self.normalize_tool_search_call(item)?));
                }
                "tool_search_output" => {
                    let normalized = self.normalize_tool_search_output(item)?;
                    rewritten.push(Value::Object(normalized.history));
                    loaded_tools.extend(normalized.loaded_tools);
                    visible_tools.extend(normalized.visible_tools);
                }
                "custom_tool_call" => {
                    rewritten.push(Value::Object(self.normalize_custom_tool_call_input(item)?));
                }
                "custom_tool_call_output" => rewritten.push(Value::Object(
                    self.normalize_function_call_output_input(item, false)?,
                )),
                "apply_patch_call" => {
                    rewritten.push(Value::Object(self.normalize_apply_patch_call_input(item)?));
                }
                "apply_patch_call_output" => {
                    rewritten.push(Value::Object(normalize_apply_patch_output_input(item)?));
                }
                "agent_message" => rewritten.push(normalize_agent_message_input(item)),
                "local_shell_call" => {
                    rewritten.push(Value::Object(normalize_legacy_local_shell_call_input(
                        item,
                    )?));
                }
                "local_shell_call_output" => rewritten.push(Value::Object(
                    normalize_legacy_local_shell_output_input(item)?,
                )),
                "shell_call_output" => {
                    rewritten.push(Value::Object(normalize_shell_call_output_input(item)?));
                }
                "mcp_tool_call_output" => rewritten.push(normalize_mcp_output_input(item)?),
                "compaction_trigger" => {
                    return Err(GrokRequestEncodeError::InvalidRequestNormalization);
                }
                "additional_tools" => {
                    let (tools, visible) = self.normalize_additional_tools_input(item)?;
                    loaded_tools.extend(tools);
                    visible_tools.extend(visible);
                }
                "" => rewritten.push(raw_item.clone()),
                unsupported => {
                    rewritten.push(unsupported_input_history_boundary(item, unsupported))
                }
            }
        }
        Ok(NormalizedInputItems {
            items: rewritten,
            loaded_tools,
            visible_tools,
        })
    }

    fn normalize_message_input(
        &mut self,
        item: &Map<String, Value>,
    ) -> Result<Map<String, Value>, GrokRequestEncodeError> {
        let role = match string_field(item, "role").trim() {
            "" => "assistant",
            "model" => "assistant",
            role => role,
        };
        let content = self.normalize_message_content(item.get("content"), role)?;
        // 透明代理合法形态：只覆盖归一化字段并剥离 Grok 注入键，未知官方
        // 字段原样保留。
        let mut converted = item.clone();
        strip_grok_internal_keys(&mut converted);
        converted.insert("type".to_owned(), Value::String("message".to_owned()));
        converted.insert("role".to_owned(), Value::String(role.to_owned()));
        converted.insert("content".to_owned(), content);
        Ok(converted)
    }

    fn normalize_message_content(
        &mut self,
        value: Option<&Value>,
        role: &str,
    ) -> Result<Value, GrokRequestEncodeError> {
        if let Some(Value::String(text)) = value {
            return Ok(Value::String(text.clone()));
        }
        let items = value
            .and_then(Value::as_array)
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        if role == "assistant" {
            let texts = items
                .iter()
                .map(|raw| {
                    let item = raw.as_object()?;
                    match string_field(item, "type") {
                        "text" | "input_text" | "output_text" => {
                            item.get("text").and_then(Value::as_str)
                        }
                        "refusal" => item.get("refusal").and_then(Value::as_str),
                        _ => None,
                    }
                })
                .collect::<Option<Vec<_>>>();
            if let Some(texts) = texts {
                return Ok(Value::String(texts.join("\n")));
            }
        }
        let text_part_type = if role == "assistant" {
            "output_text"
        } else {
            "input_text"
        };
        let mut normalized = Vec::with_capacity(items.len());
        for raw in items {
            let item = raw
                .as_object()
                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
            let converted = match string_field(item, "type") {
                "text" | "input_text" | "output_text" => json_object([
                    ("type", Value::String(text_part_type.to_owned())),
                    ("text", Value::String(string_field(item, "text").to_owned())),
                ]),
                "refusal" => json_object([
                    ("type", Value::String(text_part_type.to_owned())),
                    (
                        "text",
                        Value::String(string_field(item, "refusal").to_owned()),
                    ),
                ]),
                "input_image" => Value::Object(self.normalize_input_image_part(item)?),
                "input_file" => Value::Object(normalize_input_file_part(item)),
                _ => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
            };
            normalized.push(converted);
        }
        Ok(Value::Array(normalized))
    }

    fn normalize_input_image_part(
        &mut self,
        item: &Map<String, Value>,
    ) -> Result<Map<String, Value>, GrokRequestEncodeError> {
        let detail = match item.get("detail") {
            None | Some(Value::Null) => "auto",
            Some(Value::String(detail)) if detail.trim().is_empty() => "auto",
            Some(Value::String(detail)) => detail.trim(),
            Some(_) => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
        };
        let detail = match detail {
            "auto" | "low" | "high" => detail,
            "original" => "high",
            _ => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
        };
        let mut converted = Map::from_iter([
            ("type".to_owned(), Value::String("input_image".to_owned())),
            ("detail".to_owned(), Value::String(detail.to_owned())),
        ]);
        if let Some(value) = item.get("image_url").or_else(|| item.get("url"))
            && !value.is_null()
        {
            converted.insert("image_url".to_owned(), value.clone());
        }
        if let Some(value) = item.get("file_id")
            && !value.is_null()
        {
            converted.insert("file_id".to_owned(), value.clone());
        }
        Ok(converted)
    }

    fn normalize_function_call_input(
        &mut self,
        item: &Map<String, Value>,
    ) -> Result<Map<String, Value>, GrokRequestEncodeError> {
        let mut name = required_trimmed_string(item, "name")?.to_owned();
        let call_id = required_trimmed_string(item, "call_id")?;
        let arguments = encode_function_arguments(item.get("arguments"))?;
        let namespace = string_field(item, "namespace").trim();
        if !namespace.is_empty() {
            name = self.alias(ToolIdentity::new(ToolKind::Function, namespace, &name));
        }
        let mut converted = item.clone();
        strip_grok_internal_keys(&mut converted);
        converted.remove("namespace");
        converted.insert("type".to_owned(), Value::String("function_call".to_owned()));
        converted.insert("call_id".to_owned(), Value::String(call_id.to_owned()));
        converted.insert("name".to_owned(), Value::String(name));
        converted.insert("arguments".to_owned(), Value::String(arguments));
        Ok(converted)
    }

    pub(super) fn normalize_custom_tool_call_input(
        &mut self,
        item: &Map<String, Value>,
    ) -> Result<Map<String, Value>, GrokRequestEncodeError> {
        let name = required_trimmed_string(item, "name")?;
        let input = item
            .get("input")
            .and_then(Value::as_str)
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        let call_id = required_trimmed_string(item, "call_id")?;
        let namespace = string_field(item, "namespace").trim();
        let identity = ToolIdentity::new(ToolKind::Custom, namespace, name);
        let arguments = serde_json::to_string(&json_object([(
            identity.custom_argument_field(),
            Value::String(input.to_owned()),
        )]))
        .map_err(|_| GrokRequestEncodeError::InvalidRequestNormalization)?;
        let alias = self.alias(identity);
        let mut converted = item.clone();
        strip_grok_internal_keys(&mut converted);
        converted.remove("namespace");
        converted.remove("input");
        converted.insert("type".to_owned(), Value::String("function_call".to_owned()));
        converted.insert("call_id".to_owned(), Value::String(call_id.to_owned()));
        converted.insert("name".to_owned(), Value::String(alias));
        converted.insert("arguments".to_owned(), Value::String(arguments));
        Ok(converted)
    }

    fn normalize_function_call_output_input(
        &mut self,
        item: &Map<String, Value>,
        allow_content_blocks: bool,
    ) -> Result<Map<String, Value>, GrokRequestEncodeError> {
        let call_id = required_trimmed_string(item, "call_id")?;
        let output = match item.get("output") {
            Some(Value::Array(blocks))
                if allow_content_blocks && is_function_output_content_array(blocks) =>
            {
                Value::Array(self.normalize_function_output_blocks(blocks)?)
            }
            output => Value::String(encode_tool_output(output)?),
        };
        let mut converted = item.clone();
        strip_grok_internal_keys(&mut converted);
        converted.insert(
            "type".to_owned(),
            Value::String("function_call_output".to_owned()),
        );
        converted.insert("call_id".to_owned(), Value::String(call_id.to_owned()));
        converted.insert("output".to_owned(), output);
        Ok(converted)
    }

    fn normalize_function_output_blocks(
        &mut self,
        blocks: &[Value],
    ) -> Result<Vec<Value>, GrokRequestEncodeError> {
        blocks
            .iter()
            .map(|raw| {
                let block = raw
                    .as_object()
                    .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
                match string_field(block, "type") {
                    "input_text" => Ok(json_object([
                        ("type", Value::String("input_text".to_owned())),
                        (
                            "text",
                            Value::String(
                                block
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?
                                    .to_owned(),
                            ),
                        ),
                    ])),
                    "input_image" => {
                        require_content_source(block, &["image_url", "file_id"])?;
                        self.normalize_input_image_part(block).map(Value::Object)
                    }
                    "input_file" => {
                        require_content_source(block, &["file_data", "file_id", "file_url"])?;
                        Ok(Value::Object(normalize_input_file_part(block)))
                    }
                    _ => Err(GrokRequestEncodeError::InvalidRequestNormalization),
                }
            })
            .collect()
    }

    fn normalize_tool_search_call(
        &mut self,
        item: &Map<String, Value>,
    ) -> Result<Map<String, Value>, GrokRequestEncodeError> {
        let call_id = required_trimmed_string(item, "call_id")?;
        let execution = string_field(item, "execution").trim().to_ascii_lowercase();
        if execution.is_empty() || execution == "server" {
            self.server_search_eager = true;
            return Ok(boundary_message(
                "A server-side tool search occurred here; selected tools are made available directly.",
            ));
        }
        if execution != "client" {
            return Err(GrokRequestEncodeError::InvalidRequestNormalization);
        }
        let arguments = encode_function_arguments(item.get("arguments"))?;
        let alias = self.alias(ToolIdentity::new(ToolKind::ToolSearch, "", "tool_search"));
        Ok(Map::from_iter([
            ("type".to_owned(), Value::String("function_call".to_owned())),
            ("call_id".to_owned(), Value::String(call_id.to_owned())),
            ("name".to_owned(), Value::String(alias)),
            ("arguments".to_owned(), Value::String(arguments)),
        ]))
    }

    fn normalize_tool_search_output(
        &mut self,
        item: &Map<String, Value>,
    ) -> Result<NormalizedToolSearchOutput, GrokRequestEncodeError> {
        let execution = string_field(item, "execution").trim().to_ascii_lowercase();
        if !matches!(execution.as_str(), "" | "client" | "server") {
            return Err(GrokRequestEncodeError::InvalidRequestNormalization);
        }
        let call_id = required_trimmed_string(item, "call_id")?;
        let tools = item
            .get("tools")
            .and_then(Value::as_array)
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        let mut normalized = Vec::new();
        for tool in tools {
            normalized.extend(self.normalize_tool(tool, "", false, true)?);
        }
        let message = format!(
            "Tool search completed; {} selected tool definitions are now available.",
            tools.len()
        );
        let history = if execution == "client" {
            Map::from_iter([
                (
                    "type".to_owned(),
                    Value::String("function_call_output".to_owned()),
                ),
                ("call_id".to_owned(), Value::String(call_id.to_owned())),
                ("output".to_owned(), Value::String(message)),
            ])
        } else {
            self.server_search_eager = true;
            boundary_message(&message)
        };
        Ok(NormalizedToolSearchOutput {
            history,
            loaded_tools: normalized,
            visible_tools: tools.clone(),
        })
    }

    fn normalize_apply_patch_call_input(
        &mut self,
        item: &Map<String, Value>,
    ) -> Result<Map<String, Value>, GrokRequestEncodeError> {
        let call_id = required_trimmed_string(item, "call_id")?;
        let operation = validate_apply_patch_operation(item.get("operation"))?;
        let arguments =
            serde_json::to_string(&json_object([("operation", Value::Object(operation))]))
                .map_err(|_| GrokRequestEncodeError::InvalidRequestNormalization)?;
        let alias = self.alias(ToolIdentity::new(ToolKind::ApplyPatch, "", "apply_patch"));
        Ok(Map::from_iter([
            ("type".to_owned(), Value::String("function_call".to_owned())),
            ("call_id".to_owned(), Value::String(call_id.to_owned())),
            ("name".to_owned(), Value::String(alias)),
            ("arguments".to_owned(), Value::String(arguments)),
        ]))
    }

    fn normalize_additional_tools_input(
        &mut self,
        item: &Map<String, Value>,
    ) -> Result<(Vec<Value>, Vec<Value>), GrokRequestEncodeError> {
        let tools = item
            .get("tools")
            .and_then(Value::as_array)
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        let mut normalized = Vec::new();
        for raw_tool in tools {
            normalized.extend(self.normalize_tool(raw_tool, "", false, true)?);
        }
        Ok((normalized, tools.clone()))
    }
}

pub(super) fn normalize_input_file_part(item: &Map<String, Value>) -> Map<String, Value> {
    let mut converted =
        Map::from_iter([("type".to_owned(), Value::String("input_file".to_owned()))]);
    for key in ["file_data", "file_id", "filename", "file_url"] {
        if let Some(value) = item.get(key)
            && !value.is_null()
        {
            converted.insert(key.to_owned(), value.clone());
        }
    }
    converted
}

pub(super) fn required_trimmed_string<'a>(
    item: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a str, GrokRequestEncodeError> {
    item.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)
}

pub(super) fn encode_function_arguments(
    value: Option<&Value>,
) -> Result<String, GrokRequestEncodeError> {
    match value {
        Some(Value::String(value)) => Ok(value.clone()),
        None | Some(Value::Null) => Ok("{}".to_owned()),
        Some(value) => serde_json::to_string(value)
            .map_err(|_| GrokRequestEncodeError::InvalidRequestNormalization),
    }
}

pub(super) fn normalize_function_arguments(arguments: &str, schema: &Value) -> Option<String> {
    if arguments.trim().is_empty() || !schema.is_object() {
        return None;
    }
    let mut value = serde_json::from_str::<Value>(arguments).ok()?;
    if !normalize_argument_value(&mut value, schema, schema, 0) {
        return None;
    }
    serde_json::to_string(&value).ok()
}

pub(super) fn normalize_argument_value(
    value: &mut Value,
    schema: &Value,
    root: &Value,
    depth: usize,
) -> bool {
    if depth > 64 {
        return false;
    }
    let Some(schema) = schema.as_object() else {
        return false;
    };
    let mut changed = false;
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
        && let Some(resolved) = resolve_local_schema_value_ref(root, reference)
    {
        changed |= normalize_argument_value(value, resolved, root, depth + 1);
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
            for branch in branches {
                changed |= normalize_argument_value(value, branch, root, depth + 1);
            }
        }
    }
    if schema_requires_integer(schema)
        && let Value::Number(number) = value
        && let Some(normalized) = normalize_integral_number(number)
    {
        *number = normalized;
        return true;
    }
    match value {
        Value::Object(object) => {
            let properties = schema.get("properties").and_then(Value::as_object);
            let additional = schema
                .get("additionalProperties")
                .filter(|value| value.is_object());
            for (key, item) in object {
                let property = properties
                    .and_then(|properties| properties.get(key))
                    .or(additional);
                if let Some(property) = property {
                    changed |= normalize_argument_value(item, property, root, depth + 1);
                }
            }
        }
        Value::Array(items) => {
            let prefix = schema.get("prefixItems").and_then(Value::as_array);
            let item_schema = schema.get("items").filter(|value| value.is_object());
            for (index, item) in items.iter_mut().enumerate() {
                let schema = prefix
                    .and_then(|prefix| prefix.get(index))
                    .filter(|value| value.is_object())
                    .or(item_schema);
                if let Some(schema) = schema {
                    changed |= normalize_argument_value(item, schema, root, depth + 1);
                }
            }
        }
        _ => {}
    }
    changed
}

pub(super) fn schema_requires_integer(schema: &Map<String, Value>) -> bool {
    match schema.get("type") {
        Some(Value::String(kind)) => kind == "integer",
        Some(Value::Array(kinds)) => {
            let mut integer = false;
            for kind in kinds.iter().filter_map(Value::as_str) {
                if kind == "number" {
                    return false;
                }
                integer |= kind == "integer";
            }
            integer
        }
        _ => false,
    }
}

pub(super) fn normalize_integral_number(number: &serde_json::Number) -> Option<serde_json::Number> {
    let raw = number.to_string();
    if raw.len() > MAX_JSON_NUMBER_TEXT_BYTES || !raw.contains(['.', 'e', 'E']) {
        return None;
    }
    let (mantissa, exponent_text) = raw.find(['e', 'E']).map_or((raw.as_str(), ""), |index| {
        (&raw[..index], &raw[index + 1..])
    });
    let (negative, mantissa) = mantissa
        .strip_prefix('-')
        .map_or((false, mantissa), |value| (true, value));
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits = format!("{whole}{fraction}")
        .trim_start_matches('0')
        .to_owned();
    if digits.is_empty() {
        return "0".parse().ok().filter(|_| raw != "0");
    }
    let exponent = parse_bounded_decimal_exponent(exponent_text)?;
    let decimal_shift = exponent - i64::try_from(fraction.len()).ok()?;
    if decimal_shift < 0 {
        let fractional_digits = usize::try_from(-decimal_shift).ok()?;
        if fractional_digits > digits.len()
            || !digits[digits.len() - fractional_digits..]
                .bytes()
                .all(|byte| byte == b'0')
        {
            return None;
        }
        digits.truncate(digits.len() - fractional_digits);
        let trimmed = digits.trim_start_matches('0');
        if trimmed.is_empty() {
            return "0".parse().ok();
        }
        digits = trimmed.to_owned();
    } else if decimal_shift > 0 {
        let shift = usize::try_from(decimal_shift).ok()?;
        if shift
            > MAX_EXACT_JSON_INTEGER_TEXT
                .len()
                .saturating_sub(digits.len())
        {
            return None;
        }
        digits.extend(std::iter::repeat_n('0', shift));
    }
    if digits.len() > MAX_EXACT_JSON_INTEGER_TEXT.len()
        || (digits.len() == MAX_EXACT_JSON_INTEGER_TEXT.len()
            && digits.as_str() > MAX_EXACT_JSON_INTEGER_TEXT)
    {
        return None;
    }
    let normalized = if negative {
        format!("-{digits}")
    } else {
        digits
    };
    if normalized == raw {
        None
    } else {
        normalized.parse().ok()
    }
}

pub(super) fn exact_nonnegative_sequence(value: &Value) -> Option<u64> {
    let number = value.as_number()?;
    let sequence = number
        .as_u64()
        .or_else(|| normalize_integral_number(number)?.as_u64())?;
    (sequence <= MAX_EXACT_JSON_INTEGER).then_some(sequence)
}

pub(super) fn parse_bounded_decimal_exponent(raw: &str) -> Option<i64> {
    if raw.is_empty() {
        return Some(0);
    }
    let (sign, digits) = if let Some(value) = raw.strip_prefix('+') {
        (1_i64, value)
    } else if let Some(value) = raw.strip_prefix('-') {
        (-1_i64, value)
    } else {
        (1_i64, raw)
    };
    if digits.is_empty() {
        return None;
    }
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some(0);
    }
    if digits.len() > 9 {
        return None;
    }
    digits.parse::<i64>().ok().map(|value| sign * value)
}

pub(super) fn resolve_local_schema_value_ref<'a>(
    root: &'a Value,
    reference: &str,
) -> Option<&'a Value> {
    if reference == "#" {
        return Some(root);
    }
    let pointer = reference.strip_prefix('#')?;
    if !pointer.starts_with('/') {
        return None;
    }
    root.pointer(pointer)
}

pub(super) fn schema_contains_integer(schema: &Value, depth: usize) -> bool {
    let mut visited = BTreeSet::new();
    schema_contains_reachable_integer(schema, schema, &mut visited, depth)
}

pub(super) fn schema_contains_reachable_integer(
    schema: &Value,
    root: &Value,
    visited_refs: &mut BTreeSet<String>,
    depth: usize,
) -> bool {
    if depth > 64 {
        return false;
    }
    let Some(schema) = schema.as_object() else {
        return false;
    };
    if schema_requires_integer(schema) {
        return true;
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
        && visited_refs.insert(reference.to_owned())
        && let Some(resolved) = resolve_local_schema_value_ref(root, reference)
        && schema_contains_reachable_integer(resolved, root, visited_refs, depth + 1)
    {
        return true;
    }
    for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if schema
            .get(keyword)
            .and_then(Value::as_array)
            .is_some_and(|branches| {
                branches.iter().any(|branch| {
                    schema_contains_reachable_integer(branch, root, visited_refs, depth + 1)
                })
            })
        {
            return true;
        }
    }
    for keyword in ["items", "additionalProperties"] {
        if schema.get(keyword).is_some_and(|child| {
            schema_contains_reachable_integer(child, root, visited_refs, depth + 1)
        }) {
            return true;
        }
    }
    schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| {
            properties.values().any(|property| {
                schema_contains_reachable_integer(property, root, visited_refs, depth + 1)
            })
        })
}

pub(super) fn encode_tool_output(value: Option<&Value>) -> Result<String, GrokRequestEncodeError> {
    match value {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(value)) => Ok(value.clone()),
        Some(value) => serde_json::to_string(value)
            .map_err(|_| GrokRequestEncodeError::InvalidRequestNormalization),
    }
}

pub(super) fn is_function_output_content_array(blocks: &[Value]) -> bool {
    blocks.iter().any(|raw| {
        raw.pointer("/type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.starts_with("input_"))
    })
}
