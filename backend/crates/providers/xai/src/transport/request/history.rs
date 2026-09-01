//! OpenAI history 到 Grok Build history 的重建与清理。

use super::*;

pub(super) fn require_content_source(
    block: &Map<String, Value>,
    fields: &[&str],
) -> Result<(), GrokRequestEncodeError> {
    let has_source = fields.iter().any(|field| {
        block
            .get(*field)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    });
    has_source
        .then_some(())
        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)
}

pub(super) fn sanitize_reasoning_input(item: &Map<String, Value>) -> Map<String, Value> {
    let mut converted =
        copy_non_null_history_fields(item, &["id", "summary", "content", "encrypted_content"]);
    // Grok CLI 会在 summary/content 条目上注入 phase 等内部键。
    strip_grok_internal_entry_keys(&mut converted, &["summary", "content"]);
    converted.insert("type".to_owned(), Value::String("reasoning".to_owned()));
    if has_portable_reasoning_content(&converted) {
        converted
    } else {
        boundary_message(
            "A prior model reasoning item was omitted because it has no portable content for Grok Build.",
        )
    }
}

pub(super) fn normalize_compaction_input(
    item: &Map<String, Value>,
) -> Result<Vec<Value>, GrokRequestEncodeError> {
    let encrypted = item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let has_structured_compaction_shape = item.contains_key("summary")
        || item.contains_key("id")
        || item.contains_key("status")
        || string_field(item, "type").trim() == "compaction_summary";

    // 旧版 adapter 曾将明文摘要直接写入 encrypted_content，且不带
    // id/status/summary。仅对这一可识别的历史形态保留明文恢复；新形态严格回放为
    // xAI reasoning 密文 + 可见 conversation summary。
    if !has_structured_compaction_shape {
        let Some(summary) = encrypted else {
            return Ok(Vec::new());
        };
        let continuation = format!(
            "This session is being continued from a previous conversation that ran out of context. \
             The summary below covers the earlier portion of the conversation.\n\n{summary}"
        );
        return Ok(vec![Value::Object(compaction_summary_message(
            &continuation,
        ))]);
    }

    let mut converted = Vec::with_capacity(2);
    if let Some(encrypted) = encrypted {
        converted.push(json!({
            "type": "reasoning",
            "summary": [],
            "encrypted_content": encrypted,
        }));
    }
    if let Some(summary) = compact_summary_text(item.get("summary")) {
        converted.push(Value::Object(compaction_summary_message(&format!(
            "<conversation_summary>\n{summary}\n</conversation_summary>"
        ))));
    }
    Ok(converted)
}

pub(super) fn compact_summary_text(value: Option<&Value>) -> Option<String> {
    let text = value?
        .as_array()?
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

pub(super) fn compaction_summary_message(text: &str) -> Map<String, Value> {
    Map::from_iter([
        ("type".to_owned(), Value::String("message".to_owned())),
        ("role".to_owned(), Value::String("user".to_owned())),
        (
            "content".to_owned(),
            Value::Array(vec![json_object([
                ("type", Value::String("input_text".to_owned())),
                ("text", Value::String(text.to_owned())),
            ])]),
        ),
    ])
}

pub(in crate::transport) fn strip_invalid_encrypted_reasoning_from_body(
    body: &mut Map<String, Value>,
) -> bool {
    let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return false;
    };
    let has_encrypted_reasoning = input.iter().any(|item| {
        item.as_object().is_some_and(|item| {
            string_field(item, "type").trim() == "reasoning"
                && item
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.trim().is_empty())
        })
    });
    if !has_encrypted_reasoning {
        return false;
    }

    input.retain_mut(|item| {
        let Some(item) = item.as_object_mut() else {
            return true;
        };
        if string_field(item, "type").trim() != "reasoning" {
            return true;
        }
        item.remove("encrypted_content");
        if item.get("content").is_some_and(Value::is_null) {
            item.remove("content");
        }
        item.len() > 1
    });
    true
}

pub(super) fn has_portable_reasoning_content(item: &Map<String, Value>) -> bool {
    item.get("encrypted_content")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
        || ["summary", "content"].into_iter().any(|field| {
            item.get(field)
                .and_then(Value::as_array)
                .is_some_and(|values| !values.is_empty())
        })
}

pub(super) fn sanitize_native_history_input(
    item: &Map<String, Value>,
    item_type: &str,
) -> Map<String, Value> {
    let fields: &[&str] = match item_type {
        "file_search_call" => &["id", "queries", "status", "results"],
        "web_search_call" => &["action", "id", "status"],
        "image_generation_call" => &["id", "result", "status"],
        "code_interpreter_call" => &["code", "container_id", "id", "outputs", "status"],
        "shell_call" => &["id", "call_id", "action", "status", "environment"],
        "mcp_list_tools" => &["id", "server_label", "tools", "error"],
        "mcp_approval_request" => &["arguments", "id", "name", "server_label"],
        "mcp_approval_response" => &["approval_request_id", "approve", "id", "reason"],
        "mcp_call" => &[
            "arguments",
            "id",
            "name",
            "server_label",
            "approval_request_id",
            "error",
            "output",
            "status",
        ],
        _ => &[],
    };
    let mut converted = copy_non_null_history_fields(item, fields);
    // Grok CLI 会在 shell_call 的 action object 里注入内部键与 null 占位
    // 字段（如 `timeout_ms: null`）；只在这一层剥离，深层内容原样保留。
    if item_type == "shell_call"
        && let Some(Value::Object(action)) = converted.get_mut("action")
    {
        strip_grok_internal_keys(action);
        action.retain(|_, value| !value.is_null());
    }
    converted.insert("type".to_owned(), Value::String(item_type.to_owned()));
    converted
}

pub(super) fn copy_non_null_history_fields(
    item: &Map<String, Value>,
    fields: &[&str],
) -> Map<String, Value> {
    fields
        .iter()
        .filter_map(|field| {
            item.get(*field)
                .filter(|value| !value.is_null())
                .map(|value| ((*field).to_owned(), value.clone()))
        })
        .collect()
}

pub(super) fn strip_grok_internal_keys(object: &mut Map<String, Value>) {
    for key in GROK_INTERNAL_HISTORY_KEYS {
        object.remove(*key);
    }
}

pub(super) fn strip_grok_internal_entry_keys(converted: &mut Map<String, Value>, fields: &[&str]) {
    for field in fields {
        let Some(Value::Array(entries)) = converted.get_mut(*field) else {
            continue;
        };
        for entry in entries {
            if let Some(object) = entry.as_object_mut() {
                strip_grok_internal_keys(object);
            }
        }
    }
}

pub(super) fn unsupported_input_history_boundary(item: &Map<String, Value>, kind: &str) -> Value {
    let mut lines = vec![
        "A prior Responses history item was omitted because Grok Build cannot deserialize this Codex item type."
            .to_owned(),
        format!("Type: {kind}"),
    ];
    for key in ["id", "call_id", "name", "status"] {
        let value = string_field(item, key).trim();
        if !value.is_empty() {
            lines.push(format!("{}: {value}", key.replace('_', " ")));
        }
    }
    Value::Object(boundary_message(&lines.join("\n")))
}

pub(super) fn boundary_message(text: &str) -> Map<String, Value> {
    Map::from_iter([
        ("type".to_owned(), Value::String("message".to_owned())),
        ("role".to_owned(), Value::String("developer".to_owned())),
        (
            "content".to_owned(),
            Value::Array(vec![json_object([
                ("type", Value::String("input_text".to_owned())),
                ("text", Value::String(text.to_owned())),
            ])]),
        ),
    ])
}

pub(super) fn normalize_agent_message_input(item: &Map<String, Value>) -> Value {
    let Some(content) = item.get("content").and_then(text_input_content) else {
        return Value::Object(boundary_message(
            "An encrypted inter-agent message occurred here but is not portable to the Grok Build account.",
        ));
    };
    let author = non_empty_or(string_field(item, "author"), "agent");
    let recipient = non_empty_or(string_field(item, "recipient"), "recipient");
    Value::Object(boundary_message(&format!(
        "Agent message ({author} -> {recipient}):\n{content}"
    )))
}

pub(super) fn text_input_content(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                let item = item.as_object()?;
                matches!(
                    string_field(item, "type"),
                    "input_text" | "output_text" | "text"
                )
                .then(|| string_field(item, "text").to_owned())
            })
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.join("\n")),
        _ => None,
    }
}

pub(super) fn non_empty_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    let value = value.trim();
    if value.is_empty() { fallback } else { value }
}

pub(super) fn normalize_mcp_output_input(
    item: &Map<String, Value>,
) -> Result<Value, GrokRequestEncodeError> {
    let output = serde_json::to_string(item.get("output").unwrap_or(&Value::Null))
        .map_err(|_| GrokRequestEncodeError::InvalidRequestNormalization)?;
    let call_id = non_empty_or(string_field(item, "call_id"), "unknown");
    Ok(Value::Object(boundary_message(&format!(
        "MCP tool output for call {call_id}: {output}"
    ))))
}

pub(super) fn validate_apply_patch_operation(
    value: Option<&Value>,
) -> Result<Map<String, Value>, GrokRequestEncodeError> {
    let operation = value
        .and_then(Value::as_object)
        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
    let kind = required_trimmed_string(operation, "type")?;
    required_trimmed_string(operation, "path")?;
    match kind {
        "create_file" | "update_file" if operation.get("diff").is_some_and(Value::is_string) => {}
        "delete_file" => {}
        _ => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
    }
    Ok(operation.clone())
}

pub(super) fn normalize_apply_patch_output_input(
    item: &Map<String, Value>,
) -> Result<Map<String, Value>, GrokRequestEncodeError> {
    let call_id = required_trimmed_string(item, "call_id")?;
    let status = match string_field(item, "status").trim() {
        "" | "completed" => "completed",
        "failed" => "failed",
        _ => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
    };
    let output = encode_tool_output(item.get("output"))?;
    let mut message = format!("Apply patch status: {status}");
    if !output.is_empty() {
        message.push('\n');
        message.push_str(&output);
    }
    Ok(Map::from_iter([
        (
            "type".to_owned(),
            Value::String("function_call_output".to_owned()),
        ),
        ("call_id".to_owned(), Value::String(call_id.to_owned())),
        ("output".to_owned(), Value::String(message)),
    ]))
}

pub(super) fn normalize_legacy_local_shell_call_input(
    item: &Map<String, Value>,
) -> Result<Map<String, Value>, GrokRequestEncodeError> {
    let call_id = required_trimmed_string(item, "call_id")?;
    let action = legacy_shell_action(item.get("action"))?;
    let mut converted = Map::from_iter([
        ("type".to_owned(), Value::String("shell_call".to_owned())),
        ("call_id".to_owned(), Value::String(call_id.to_owned())),
        ("action".to_owned(), Value::Object(action)),
    ]);
    for key in ["id", "status", "timeout_ms", "max_output_length"] {
        if let Some(value) = item.get(key) {
            converted.insert(key.to_owned(), value.clone());
        }
    }
    Ok(converted)
}

pub(super) fn legacy_shell_action(
    value: Option<&Value>,
) -> Result<Map<String, Value>, GrokRequestEncodeError> {
    let action = value
        .and_then(Value::as_object)
        .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
    if !matches!(string_field(action, "type").trim(), "" | "exec") {
        return Err(GrokRequestEncodeError::InvalidRequestNormalization);
    }
    let command = legacy_shell_command(action)?;
    Ok(Map::from_iter([
        ("type".to_owned(), Value::String("exec".to_owned())),
        (
            "commands".to_owned(),
            Value::Array(vec![Value::String(command)]),
        ),
    ]))
}

pub(super) fn legacy_shell_command(
    action: &Map<String, Value>,
) -> Result<String, GrokRequestEncodeError> {
    let mut command = match action.get("command") {
        Some(Value::String(value)) => value.trim().to_owned(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| {
                part.as_str()
                    .map(quote_shell_argument)
                    .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)
            })
            .collect::<Result<Vec<_>, _>>()?
            .join(" "),
        _ => action
            .get("commands")
            .and_then(Value::as_array)
            .map(|commands| {
                commands
                    .iter()
                    .map(|command| {
                        command
                            .as_str()
                            .map(ToOwned::to_owned)
                            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(|commands| commands.join("\n"))
            })
            .transpose()?
            .unwrap_or_default(),
    };
    if command.is_empty() {
        return Err(GrokRequestEncodeError::InvalidRequestNormalization);
    }
    if let Some(environment) = action.get("env").and_then(Value::as_object)
        && !environment.is_empty()
    {
        let assignments = environment
            .iter()
            .map(|(name, value)| {
                if !valid_environment_name(name) {
                    return Err(GrokRequestEncodeError::InvalidRequestNormalization);
                }
                let value = value
                    .as_str()
                    .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
                Ok(format!("{name}={}", quote_shell_argument(value)))
            })
            .collect::<Result<Vec<_>, _>>()?;
        command = format!("env {} {command}", assignments.join(" "));
    }
    if let Some(directory) = action
        .get("working_directory")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|directory| !directory.is_empty())
    {
        command = format!("cd {} && {command}", quote_shell_argument(directory));
    }
    Ok(command)
}

pub(super) fn normalize_legacy_local_shell_output_input(
    item: &Map<String, Value>,
) -> Result<Map<String, Value>, GrokRequestEncodeError> {
    let call_id = required_trimmed_string(item, "call_id")?;
    let output = match item.get("output") {
        Some(Value::Array(output)) => output.clone(),
        Some(Value::String(output)) => {
            let exit_code = item
                .get("exit_code")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| {
                    i64::from(string_field(item, "status").eq_ignore_ascii_case("failed"))
                });
            vec![shell_output_block(output, "", "exit", Some(exit_code))]
        }
        _ => return Err(GrokRequestEncodeError::InvalidRequestNormalization),
    };
    let mut converted = Map::from_iter([
        (
            "type".to_owned(),
            Value::String("shell_call_output".to_owned()),
        ),
        ("call_id".to_owned(), Value::String(call_id.to_owned())),
        ("output".to_owned(), Value::Array(output)),
    ]);
    if let Some(value) = item.get("max_output_length")
        && !value.is_null()
    {
        converted.insert("max_output_length".to_owned(), value.clone());
    }
    Ok(converted)
}

pub(super) fn normalize_shell_call_output_input(
    item: &Map<String, Value>,
) -> Result<Map<String, Value>, GrokRequestEncodeError> {
    let call_id = required_trimmed_string(item, "call_id")?;
    let output = normalize_shell_output_blocks(item.get("output"), item.get("status"))?;
    let mut converted = Map::from_iter([
        (
            "type".to_owned(),
            Value::String("shell_call_output".to_owned()),
        ),
        ("call_id".to_owned(), Value::String(call_id.to_owned())),
        ("output".to_owned(), Value::Array(output)),
    ]);
    if let Some(value) = item.get("max_output_length")
        && !value.is_null()
    {
        converted.insert("max_output_length".to_owned(), value.clone());
    }
    Ok(converted)
}

pub(super) fn normalize_shell_output_blocks(
    value: Option<&Value>,
    status: Option<&Value>,
) -> Result<Vec<Value>, GrokRequestEncodeError> {
    match value {
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|raw| {
                let block = raw
                    .as_object()
                    .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
                let stdout = string_field(block, "stdout");
                let stderr = string_field(block, "stderr");
                let outcome = block
                    .get("outcome")
                    .and_then(Value::as_object)
                    .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
                match string_field(outcome, "type").trim() {
                    "exit" => {
                        let exit_code = outcome
                            .get("exit_code")
                            .or_else(|| outcome.get("exitCode"))
                            .and_then(Value::as_i64)
                            .ok_or(GrokRequestEncodeError::InvalidRequestNormalization)?;
                        Ok(shell_output_block(stdout, stderr, "exit", Some(exit_code)))
                    }
                    "timeout" => Ok(shell_output_block(stdout, stderr, "timeout", None)),
                    _ => Err(GrokRequestEncodeError::InvalidRequestNormalization),
                }
            })
            .collect(),
        Some(Value::String(output)) => {
            let failed = status
                .and_then(Value::as_str)
                .is_some_and(|status| status.eq_ignore_ascii_case("failed"));
            Ok(vec![shell_output_block(
                output,
                "",
                "exit",
                Some(i64::from(failed)),
            )])
        }
        _ => Err(GrokRequestEncodeError::InvalidRequestNormalization),
    }
}

pub(super) fn shell_output_block(
    stdout: &str,
    stderr: &str,
    outcome_type: &str,
    exit_code: Option<i64>,
) -> Value {
    let mut outcome = Map::from_iter([("type".to_owned(), Value::String(outcome_type.to_owned()))]);
    if let Some(exit_code) = exit_code {
        outcome.insert("exit_code".to_owned(), Value::from(exit_code));
    }
    json_object([
        ("stdout", Value::String(stdout.to_owned())),
        ("stderr", Value::String(stderr.to_owned())),
        ("outcome", Value::Object(outcome)),
    ])
}

pub(super) fn quote_shell_argument(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "_@%+=:,./-".contains(character))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub(super) fn valid_environment_name(value: &str) -> bool {
    value.chars().enumerate().all(|(index, character)| {
        character.is_ascii_alphabetic()
            || character == '_'
            || (index > 0 && character.is_ascii_digit())
    }) && !value.is_empty()
}
