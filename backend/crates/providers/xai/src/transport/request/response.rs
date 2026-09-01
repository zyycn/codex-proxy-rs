//! Grok Build 输出恢复为 OpenAI Responses wire 所需的状态。

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ToolKind {
    Function,
    Custom,
    ToolSearch,
    ApplyPatch,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ToolIdentity {
    pub(super) kind: ToolKind,
    pub(super) namespace: String,
    pub(super) name: String,
}

impl ToolIdentity {
    pub(super) fn new(kind: ToolKind, namespace: &str, name: &str) -> Self {
        Self {
            kind,
            namespace: namespace.to_owned(),
            name: name.to_owned(),
        }
    }

    pub(super) fn is_root_custom_apply_patch(&self) -> bool {
        self.kind == ToolKind::Custom && self.namespace.is_empty() && self.name == "apply_patch"
    }

    pub(super) fn custom_argument_field(&self) -> &'static str {
        if self.is_root_custom_apply_patch() {
            "patch"
        } else {
            "input"
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct StreamCallState {
    identity: ToolIdentity,
    schema: Option<Value>,
    arguments: String,
    passthrough: bool,
    last_delta: Option<Map<String, Value>>,
    added_payload: Option<Map<String, Value>>,
}

#[derive(Clone, Default)]
pub(crate) struct GrokResponseTransform {
    pub(super) aliases: BTreeMap<String, ToolIdentity>,
    pub(super) function_schemas: BTreeMap<String, Value>,
    pub(super) visible_tools: Vec<Value>,
    pub(super) legacy_local_shell: bool,
    pub(super) filter_x_search: bool,
    pub(super) injected_tool_types: BTreeSet<String>,
    pub(super) client_declared_tools: BTreeSet<String>,
    pub(super) dropped_output_indexes: BTreeSet<u64>,
    pub(super) dropped_item_ids: BTreeSet<String>,
    pub(super) stream_calls: BTreeMap<String, StreamCallState>,
    pub(super) stream_keys: BTreeMap<String, String>,
    pub(super) stream_argument_bytes: usize,
    pub(super) stream_sequence_next: Option<u64>,
}

pub(crate) struct GrokTransformedWireEvent {
    event_type: String,
    value: Value,
}

impl GrokTransformedWireEvent {
    #[must_use]
    pub(crate) fn event_type(&self) -> &str {
        &self.event_type
    }

    #[must_use]
    pub(crate) fn into_value(self) -> Value {
        self.value
    }
}

impl GrokResponseTransform {
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.aliases.is_empty()
            && self.function_schemas.is_empty()
            && self.visible_tools.is_empty()
            && !self.legacy_local_shell
            && !self.filter_x_search
            && self.injected_tool_types.is_empty()
    }

    pub(super) fn observe_client_cache_tools(&mut self) {
        for tool in &self.visible_tools {
            let kind = tool.get("type").and_then(Value::as_str).unwrap_or_default();
            if kind == "x_search" {
                self.filter_x_search = true;
            }
            if matches!(kind, "function" | "custom")
                && let Some(name) = tool
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
            {
                self.client_declared_tools.insert(name.to_owned());
            }
        }
    }

    pub(super) fn observe_upstream_cache_tools(&mut self, tools: &[Value]) {
        if has_tool_type(tools, "x_search") {
            self.filter_x_search = true;
        }
    }

    pub(super) fn mark_injected_cache_tool(&mut self, kind: &str) {
        self.injected_tool_types.insert(kind.to_owned());
        if kind == "x_search" {
            self.filter_x_search = true;
        }
    }

    pub(crate) fn resequence_stream_value(&mut self, value: &mut Value) {
        let Some(payload) = value.as_object_mut() else {
            return;
        };
        let Some(raw_sequence) = payload.get("sequence_number") else {
            return;
        };
        if self.stream_sequence_next.is_none() {
            let Some(sequence) = exact_nonnegative_sequence(raw_sequence) else {
                return;
            };
            self.stream_sequence_next = Some(sequence);
        }
        let Some(next) = self.stream_sequence_next.as_mut() else {
            return;
        };
        payload.insert(
            "sequence_number".to_owned(),
            Value::Number(serde_json::Number::from(*next)),
        );
        *next = next.saturating_add(1);
    }

    pub(crate) fn rewrite_stream_event(
        &mut self,
        event_type: &str,
        mut value: Value,
    ) -> Result<Vec<GrokTransformedWireEvent>, GrokRequestEncodeError> {
        if self.cache_filter_enabled() {
            if value
                .get("item")
                .is_some_and(|item| self.is_internal_cache_call(item))
            {
                self.record_dropped_cache_item(&value);
                return Ok(Vec::new());
            }
            self.filter_cache_envelope(&mut value)?;
            if self.references_dropped_cache_item(&value) {
                return Ok(Vec::new());
            }
            self.compact_cache_output_index(&mut value);
        }
        if event_type == "response.output_item.added"
            && let Some(item) = value.get("item").and_then(Value::as_object)
            && let Some(primary) = self.remember_stream_call(item)
            && self
                .stream_calls
                .get(&primary)
                .is_some_and(|state| state.identity.kind == ToolKind::ApplyPatch)
        {
            if let Some(state) = self.stream_calls.get_mut(&primary) {
                state.added_payload = value.as_object().cloned();
            }
            return Ok(Vec::new());
        }

        if event_type == "response.function_call_arguments.delta"
            && let Some(primary) = self.stream_call_key(&value)
        {
            let delta = value
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let state = self
                .stream_calls
                .get_mut(&primary)
                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
            match state.identity.kind {
                ToolKind::Custom => {
                    state.arguments.push_str(delta);
                    state.last_delta = value.as_object().cloned().map(|mut payload| {
                        payload.remove("delta");
                        payload
                    });
                    return Ok(Vec::new());
                }
                ToolKind::ToolSearch | ToolKind::ApplyPatch => return Ok(Vec::new()),
                ToolKind::Function if state.schema.is_some() => {
                    let call_limit_exceeded = state.arguments.len().saturating_add(delta.len())
                        > MAX_BUFFERED_FUNCTION_ARGUMENTS_BYTES;
                    let total_limit_exceeded =
                        self.stream_argument_bytes.saturating_add(delta.len())
                            > MAX_TOTAL_BUFFERED_FUNCTION_ARGUMENTS_BYTES;
                    if !state.passthrough && (call_limit_exceeded || total_limit_exceeded) {
                        let buffered = std::mem::take(&mut state.arguments);
                        self.stream_argument_bytes =
                            self.stream_argument_bytes.saturating_sub(buffered.len());
                        state.passthrough = true;
                        state.last_delta = None;
                        let mut output = Vec::with_capacity(2);
                        if !buffered.is_empty() {
                            let mut flushed = value.clone();
                            flushed
                                .as_object_mut()
                                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?
                                .insert("delta".to_owned(), Value::String(buffered));
                            output.push(GrokTransformedWireEvent {
                                event_type: event_type.to_owned(),
                                value: flushed,
                            });
                        }
                        output.push(GrokTransformedWireEvent {
                            event_type: event_type.to_owned(),
                            value,
                        });
                        return Ok(output);
                    }
                    if !state.passthrough {
                        state.arguments.push_str(delta);
                        self.stream_argument_bytes =
                            self.stream_argument_bytes.saturating_add(delta.len());
                        state.last_delta = value.as_object().cloned().map(|mut payload| {
                            payload.remove("delta");
                            payload
                        });
                        return Ok(Vec::new());
                    }
                }
                ToolKind::Function => {}
            }
        }

        if event_type == "response.function_call_arguments.done"
            && let Some(primary) = self.stream_call_key(&value)
        {
            let state = self
                .stream_calls
                .get(&primary)
                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
            match state.identity.kind {
                ToolKind::ToolSearch | ToolKind::ApplyPatch => return Ok(Vec::new()),
                ToolKind::Custom => {
                    let arguments = value
                        .get("arguments")
                        .and_then(Value::as_str)
                        .filter(|arguments| !arguments.is_empty())
                        .unwrap_or(&state.arguments);
                    let input = decode_custom_tool_input(&state.identity, arguments);
                    let mut output = Vec::with_capacity(2);
                    if let Some(delta) = state.last_delta.as_ref() {
                        output.push(GrokTransformedWireEvent {
                            event_type: "response.custom_tool_call_input.delta".to_owned(),
                            value: custom_tool_stream_payload(
                                delta,
                                "response.custom_tool_call_input.delta",
                                "delta",
                                &input,
                            ),
                        });
                    }
                    output.push(GrokTransformedWireEvent {
                        event_type: "response.custom_tool_call_input.done".to_owned(),
                        value: custom_tool_stream_payload(
                            value
                                .as_object()
                                .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?,
                            "response.custom_tool_call_input.done",
                            "input",
                            &input,
                        ),
                    });
                    if let Some(state) = self.stream_calls.get_mut(&primary) {
                        state.arguments.clear();
                        state.last_delta = None;
                    }
                    return Ok(output);
                }
                ToolKind::Function if state.schema.is_some() => {
                    let schema = state
                        .schema
                        .clone()
                        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
                    let passthrough = state.passthrough;
                    let buffered = state.arguments.clone();
                    let last_delta = state.last_delta.clone();
                    let arguments = value
                        .get("arguments")
                        .and_then(Value::as_str)
                        .filter(|arguments| !arguments.is_empty())
                        .unwrap_or(&buffered);
                    let normalized = normalize_function_arguments(arguments, &schema)
                        .unwrap_or_else(|| arguments.to_owned());
                    if passthrough {
                        value
                            .as_object_mut()
                            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?
                            .insert("arguments".to_owned(), Value::String(normalized));
                        return Ok(vec![GrokTransformedWireEvent {
                            event_type: event_type.to_owned(),
                            value,
                        }]);
                    }

                    let mut output = Vec::with_capacity(2);
                    if let Some(mut delta) = last_delta {
                        delta.insert("delta".to_owned(), Value::String(normalized.clone()));
                        output.push(GrokTransformedWireEvent {
                            event_type: "response.function_call_arguments.delta".to_owned(),
                            value: Value::Object(delta),
                        });
                    }
                    value
                        .as_object_mut()
                        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?
                        .insert("arguments".to_owned(), Value::String(normalized));
                    output.push(GrokTransformedWireEvent {
                        event_type: event_type.to_owned(),
                        value,
                    });
                    if let Some(state) = self.stream_calls.get_mut(&primary) {
                        self.stream_argument_bytes = self
                            .stream_argument_bytes
                            .saturating_sub(state.arguments.len());
                        state.arguments.clear();
                        state.last_delta = None;
                    }
                    return Ok(output);
                }
                ToolKind::Function => {}
            }
        }

        if event_type == "response.output_item.done"
            && let Some(item) = value.get("item").and_then(Value::as_object)
            && self
                .aliases
                .get(string_field(item, "name"))
                .is_some_and(|identity| identity.kind == ToolKind::ApplyPatch)
        {
            return self.rewrite_apply_patch_done_event(value);
        }

        let completed_call = (event_type == "response.output_item.done")
            .then(|| value.get("item").and_then(Value::as_object))
            .flatten()
            .and_then(|item| self.primary_stream_call(item));
        self.rewrite_response_value(&mut value)?;
        if let Some(response) = value.get_mut("response").and_then(Value::as_object_mut) {
            self.restore_visible_tools(response);
        }
        if let Some(primary) = completed_call {
            self.take_stream_call(&primary);
        }
        Ok(vec![GrokTransformedWireEvent {
            event_type: event_type.to_owned(),
            value,
        }])
    }

    fn remember_stream_call(&mut self, item: &Map<String, Value>) -> Option<String> {
        if string_field(item, "type") != "function_call" {
            return None;
        }
        let identity = self.aliases.get(string_field(item, "name"))?.clone();
        let keys = ["id", "call_id"]
            .into_iter()
            .filter_map(|field| item.get(field).and_then(Value::as_str))
            .filter(|key| !key.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let primary = keys.first()?.clone();
        self.stream_calls.insert(
            primary.clone(),
            StreamCallState {
                identity,
                schema: self
                    .function_schemas
                    .get(string_field(item, "name"))
                    .cloned(),
                arguments: String::new(),
                passthrough: false,
                last_delta: None,
                added_payload: None,
            },
        );
        for key in keys {
            self.stream_keys.insert(key, primary.clone());
        }
        Some(primary)
    }

    fn stream_call_key(&mut self, payload: &Value) -> Option<String> {
        for field in ["item_id", "call_id"] {
            if let Some(key) = payload.get(field).and_then(Value::as_str)
                && let Some(primary) = self.stream_keys.get(key)
            {
                return Some(primary.clone());
            }
        }
        let alias = payload.get("name").and_then(Value::as_str)?;
        let identity = self.aliases.get(alias)?.clone();
        let primary = ["item_id", "call_id"]
            .into_iter()
            .find_map(|field| payload.get(field).and_then(Value::as_str))?
            .to_owned();
        self.stream_calls.insert(
            primary.clone(),
            StreamCallState {
                identity,
                schema: self.function_schemas.get(alias).cloned(),
                arguments: String::new(),
                passthrough: false,
                last_delta: None,
                added_payload: None,
            },
        );
        self.stream_keys.insert(primary.clone(), primary.clone());
        Some(primary)
    }

    fn primary_stream_call(&self, item: &Map<String, Value>) -> Option<String> {
        ["id", "call_id"]
            .into_iter()
            .filter_map(|field| item.get(field).and_then(Value::as_str))
            .find_map(|key| self.stream_keys.get(key).cloned())
    }

    fn take_stream_call(&mut self, primary: &str) -> Option<StreamCallState> {
        self.stream_keys.retain(|_, owner| owner != primary);
        let state = self.stream_calls.remove(primary)?;
        self.stream_argument_bytes = self
            .stream_argument_bytes
            .saturating_sub(state.arguments.len());
        Some(state)
    }

    fn rewrite_response_value(&self, value: &mut Value) -> Result<(), GrokRequestEncodeError> {
        match value {
            Value::Array(values) => {
                for value in values {
                    self.rewrite_response_value(value)?;
                }
            }
            Value::Object(object) => {
                for value in object.values_mut() {
                    self.rewrite_response_value(value)?;
                }
                match string_field(object, "type") {
                    "function_call" => self.rewrite_function_call(object)?,
                    "shell_call" if self.legacy_local_shell => {
                        rewrite_legacy_local_shell_call(object)
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn rewrite_function_call(
        &self,
        call: &mut Map<String, Value>,
    ) -> Result<(), GrokRequestEncodeError> {
        let Some(identity) = self.aliases.get(string_field(call, "name")) else {
            return Ok(());
        };
        match identity.kind {
            ToolKind::Function => {
                let alias = string_field(call, "name").to_owned();
                if let Some(schema) = self.function_schemas.get(&alias)
                    && let Some(arguments) = call.get("arguments").and_then(Value::as_str)
                    && let Some(normalized) = normalize_function_arguments(arguments, schema)
                {
                    call.insert("arguments".to_owned(), Value::String(normalized));
                }
                call.insert("name".to_owned(), Value::String(identity.name.clone()));
                if identity.namespace.is_empty() {
                    call.remove("namespace");
                } else {
                    call.insert(
                        "namespace".to_owned(),
                        Value::String(identity.namespace.clone()),
                    );
                }
            }
            ToolKind::Custom => {
                call.insert(
                    "type".to_owned(),
                    Value::String("custom_tool_call".to_owned()),
                );
                call.insert("name".to_owned(), Value::String(identity.name.clone()));
                if identity.namespace.is_empty() {
                    call.remove("namespace");
                } else {
                    call.insert(
                        "namespace".to_owned(),
                        Value::String(identity.namespace.clone()),
                    );
                }
                let input = decode_custom_tool_input(identity, string_field(call, "arguments"));
                call.insert("input".to_owned(), Value::String(input));
                call.remove("arguments");
            }
            ToolKind::ToolSearch => {
                call.insert(
                    "type".to_owned(),
                    Value::String("tool_search_call".to_owned()),
                );
                call.insert("execution".to_owned(), Value::String("client".to_owned()));
                let arguments = decode_tool_search_arguments(call.get("arguments"));
                call.insert("arguments".to_owned(), arguments);
                call.remove("name");
                call.remove("namespace");
            }
            ToolKind::ApplyPatch => {
                let operation = decode_apply_patch_arguments(call.get("arguments"))?;
                call.insert(
                    "type".to_owned(),
                    Value::String("apply_patch_call".to_owned()),
                );
                call.insert("operation".to_owned(), Value::Object(operation));
                call.remove("name");
                call.remove("namespace");
                call.remove("arguments");
            }
        }
        Ok(())
    }

    fn rewrite_apply_patch_done_event(
        &mut self,
        mut value: Value,
    ) -> Result<Vec<GrokTransformedWireEvent>, GrokRequestEncodeError> {
        let original_item = value
            .get("item")
            .and_then(Value::as_object)
            .cloned()
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        self.rewrite_response_value(&mut value)?;
        let done = value
            .as_object()
            .cloned()
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        let done_item = done
            .get("item")
            .and_then(Value::as_object)
            .cloned()
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        let state = self
            .primary_stream_call(&original_item)
            .and_then(|primary| self.take_stream_call(&primary));
        let mut added = state
            .and_then(|state| state.added_payload)
            .unwrap_or_else(|| {
                Map::from_iter([(
                    "type".to_owned(),
                    Value::String("response.output_item.added".to_owned()),
                )])
            });
        added.insert(
            "type".to_owned(),
            Value::String("response.output_item.added".to_owned()),
        );
        for key in ["output_index", "sequence_number"] {
            if !added.contains_key(key)
                && let Some(value) = done.get(key)
            {
                added.insert(key.to_owned(), value.clone());
            }
        }
        let mut added_item = done_item;
        added_item.insert("status".to_owned(), Value::String("in_progress".to_owned()));
        added.insert("item".to_owned(), Value::Object(added_item));
        Ok(vec![
            GrokTransformedWireEvent {
                event_type: "response.output_item.added".to_owned(),
                value: Value::Object(added),
            },
            GrokTransformedWireEvent {
                event_type: "response.output_item.done".to_owned(),
                value: Value::Object(done),
            },
        ])
    }

    fn cache_filter_enabled(&self) -> bool {
        self.filter_x_search || !self.injected_tool_types.is_empty()
    }

    fn is_internal_cache_call(&self, item: &Value) -> bool {
        let Some(item) = item.as_object() else {
            return false;
        };
        let kind = string_field(item, "type").trim();
        if kind == "web_search_call" {
            return self.injected_tool_types.contains("web_search");
        }
        if !matches!(kind, "custom_tool_call" | "function_call") {
            return false;
        }
        if string_field(item, "call_id").trim().starts_with("xs_call") {
            return true;
        }
        let name = string_field(item, "name").trim();
        matches!(
            name,
            "x_user_search" | "x_semantic_search" | "x_keyword_search" | "x_thread_fetch"
        ) && string_field(item, "namespace").trim().is_empty()
            && !self.client_declared_tools.contains(name)
    }

    fn record_dropped_cache_item(&mut self, payload: &Value) {
        if let Some(index) = payload.get("output_index").and_then(Value::as_u64) {
            self.dropped_output_indexes.insert(index);
        }
        let Some(item) = payload.get("item").and_then(Value::as_object) else {
            return;
        };
        for field in ["id", "call_id"] {
            if let Some(value) = item
                .get(field)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                self.dropped_item_ids.insert(value.to_owned());
            }
        }
    }

    fn references_dropped_cache_item(&self, payload: &Value) -> bool {
        if payload
            .get("output_index")
            .and_then(Value::as_u64)
            .is_some_and(|index| self.dropped_output_indexes.contains(&index))
        {
            return true;
        }
        ["item_id", "call_id"].into_iter().any(|field| {
            payload
                .get(field)
                .and_then(Value::as_str)
                .map(str::trim)
                .is_some_and(|value| self.dropped_item_ids.contains(value))
        })
    }

    fn compact_cache_output_index(&self, payload: &mut Value) {
        let Some(index) = payload.get("output_index").and_then(Value::as_u64) else {
            return;
        };
        let removed_before = self
            .dropped_output_indexes
            .iter()
            .filter(|dropped| **dropped < index)
            .count() as u64;
        if removed_before > 0
            && let Some(payload) = payload.as_object_mut()
        {
            payload.insert(
                "output_index".to_owned(),
                Value::from(index.saturating_sub(removed_before)),
            );
        }
    }

    fn filter_cache_envelope(&self, payload: &mut Value) -> Result<(), GrokRequestEncodeError> {
        let Some(payload) = payload.as_object_mut() else {
            return Ok(());
        };
        self.filter_cache_output(payload)?;
        self.filter_injected_cache_tools(payload)?;
        if let Some(response) = payload.get_mut("response").and_then(Value::as_object_mut) {
            self.filter_cache_output(response)?;
            self.filter_injected_cache_tools(response)?;
        }
        Ok(())
    }

    fn filter_cache_output(
        &self,
        envelope: &mut Map<String, Value>,
    ) -> Result<(), GrokRequestEncodeError> {
        let Some(output) = envelope.get_mut("output") else {
            return Ok(());
        };
        let output = output
            .as_array_mut()
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        output.retain(|item| !self.is_internal_cache_call(item));
        Ok(())
    }

    fn filter_injected_cache_tools(
        &self,
        envelope: &mut Map<String, Value>,
    ) -> Result<(), GrokRequestEncodeError> {
        if self.injected_tool_types.is_empty() {
            return Ok(());
        }
        let Some(tools) = envelope.get_mut("tools") else {
            return Ok(());
        };
        let tools = tools
            .as_array_mut()
            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
        tools.retain(|tool| {
            tool.get("type")
                .and_then(Value::as_str)
                .is_none_or(|kind| !self.injected_tool_types.contains(kind.trim()))
        });
        if tools.is_empty() {
            envelope.remove("tools");
        }
        Ok(())
    }

    fn restore_visible_tools(&self, response: &mut Map<String, Value>) {
        if response.contains_key("tools") {
            if self.visible_tools.is_empty() {
                response.remove("tools");
            } else {
                response.insert("tools".to_owned(), Value::Array(self.visible_tools.clone()));
            }
        }
    }
}

pub(super) fn custom_tool_stream_payload(
    source: &Map<String, Value>,
    kind: &str,
    value_key: &str,
    value: &str,
) -> Value {
    let mut result = Map::from_iter([
        ("type".to_owned(), Value::String(kind.to_owned())),
        (value_key.to_owned(), Value::String(value.to_owned())),
    ]);
    for key in ["item_id", "output_index", "sequence_number"] {
        if let Some(value) = source.get(key) {
            result.insert(key.to_owned(), value.clone());
        }
    }
    Value::Object(result)
}

pub(super) fn decode_custom_tool_input(identity: &ToolIdentity, arguments: &str) -> String {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|value| {
            value
                .get(identity.custom_argument_field())
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| arguments.to_owned())
}

pub(super) fn decode_tool_search_arguments(value: Option<&Value>) -> Value {
    let Some(text) = value.and_then(Value::as_str) else {
        return value.cloned().unwrap_or(Value::Object(Map::new()));
    };
    if text.trim().is_empty() {
        return Value::Object(Map::new());
    }
    serde_json::from_str(text)
        .unwrap_or_else(|_| json_object([("input", Value::String(text.to_owned()))]))
}

pub(super) fn decode_apply_patch_arguments(
    value: Option<&Value>,
) -> Result<Map<String, Value>, GrokRequestEncodeError> {
    let text = value
        .and_then(Value::as_str)
        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
    let wrapper = serde_json::from_str::<Value>(text)
        .map_err(|_| GrokRequestEncodeError::InvalidRequestNormalization)?;
    validate_apply_patch_operation(wrapper.get("operation"))
}

pub(super) fn rewrite_legacy_local_shell_call(call: &mut Map<String, Value>) {
    let commands = call
        .get("action")
        .and_then(Value::as_object)
        .and_then(|action| action.get("commands"))
        .and_then(Value::as_array)
        .map(|commands| {
            commands
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    call.insert(
        "type".to_owned(),
        Value::String("local_shell_call".to_owned()),
    );
    call.insert(
        "action".to_owned(),
        json_object([
            ("type", Value::String("exec".to_owned())),
            ("command", Value::String(commands)),
        ]),
    );
    call.remove("max_output_length");
}

impl fmt::Debug for GrokResponseTransform {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokResponseTransform")
            .field("alias_count", &self.aliases.len())
            .field("function_schema_count", &self.function_schemas.len())
            .field("visible_tool_count", &self.visible_tools.len())
            .field("legacy_local_shell", &self.legacy_local_shell)
            .field("filter_x_search", &self.filter_x_search)
            .field("injected_tool_types", &self.injected_tool_types)
            .finish()
    }
}
