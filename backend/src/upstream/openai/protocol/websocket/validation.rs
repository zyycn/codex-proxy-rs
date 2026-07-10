use super::*;

/// 判断事件是否不符合官方流事件的基本字段类型。
pub fn websocket_event_shape_parse_error(raw: &str) -> bool {
    serde_json::from_str::<ResponsesStreamEventShape>(raw).is_err()
}

fn json_field_absent_or_null(value: &Value, field: &str) -> bool {
    matches!(value.get(field), None | Some(Value::Null))
}

/// 判断 `response.completed` 是否缺少 `response`。
pub fn websocket_response_completed_missing_response(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    value.get("type").and_then(Value::as_str) == Some("response.completed")
        && json_field_absent_or_null(&value, "response")
}

/// 判断 `response.created` 是否缺少 `response`。
pub fn websocket_response_created_missing_response(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    value.get("type").and_then(Value::as_str) == Some("response.created")
        && json_field_absent_or_null(&value, "response")
}

/// 判断 `response.output_text.delta` 是否缺少 `delta`。
pub fn websocket_response_output_text_delta_missing_delta(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    value.get("type").and_then(Value::as_str) == Some("response.output_text.delta")
        && json_field_absent_or_null(&value, "delta")
}

/// 判断 delta 事件是否缺少官方必需字段。
pub fn websocket_delta_event_missing_official_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    match value.get("type").and_then(Value::as_str) {
        Some("response.custom_tool_call_input.delta") => {
            value.get("delta").and_then(Value::as_str).is_none()
                || (value.get("item_id").and_then(Value::as_str).is_none()
                    && value.get("call_id").and_then(Value::as_str).is_none())
        }
        Some("response.reasoning_summary_text.delta") => {
            value.get("delta").and_then(Value::as_str).is_none()
                || value.get("summary_index").and_then(Value::as_i64).is_none()
        }
        Some("response.reasoning_text.delta") => {
            value.get("delta").and_then(Value::as_str).is_none()
                || value.get("content_index").and_then(Value::as_i64).is_none()
        }
        _ => false,
    }
}

/// 判断 output item 事件是否缺少 `item`。
pub fn websocket_output_item_event_missing_item(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    is_output_item_event(&value) && json_field_absent_or_null(&value, "item")
}

/// 判断 output item 事件的 `item` 是否不是对象。
pub fn websocket_output_item_event_non_object_item(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    is_output_item_event(&value)
        && value
            .get("item")
            .is_some_and(|item| !item.is_null() && !item.is_object())
}

/// 判断 output item 事件的 `item.type` 是否缺失。
pub fn websocket_output_item_event_invalid_item_type_tag(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| item.get("type").and_then(Value::as_str).is_none())
}

/// 判断 output item 事件的 metadata 是否无效。
pub fn websocket_output_item_event_invalid_metadata(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .and_then(|item| item.get("metadata"))
        .is_some_and(|metadata| {
            if metadata.is_null() {
                return false;
            }
            metadata
                .as_object()
                .is_none_or(|metadata| optional_string_field_invalid(metadata, "turn_id"))
        })
}

fn is_output_item_event(value: &Value) -> bool {
    matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.output_item.done" | "response.output_item.added")
    )
}

/// 判断 message output item 是否缺少官方必需字段。
pub fn websocket_message_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("message")
                && (optional_string_field_invalid(item, "id")
                    || optional_message_phase_invalid(item)
                    || item.get("role").and_then(Value::as_str).is_none()
                    || item
                        .get("content")
                        .and_then(Value::as_array)
                        .is_none_or(|content| {
                            content.iter().any(content_item_invalid_required_fields)
                        }))
        })
}

fn optional_string_field_invalid(item: &Map<String, Value>, field: &str) -> bool {
    item.get(field)
        .is_some_and(|value| !value.is_null() && !value.is_string())
}

fn optional_message_phase_invalid(item: &Map<String, Value>) -> bool {
    item.get("phase").is_some_and(|phase| {
        !phase.is_null() && !matches!(phase.as_str(), Some("commentary" | "final_answer"))
    })
}

fn content_item_invalid_required_fields(content_item: &Value) -> bool {
    let Some(content_item) = content_item.as_object() else {
        return true;
    };

    match content_item.get("type").and_then(Value::as_str) {
        Some("input_text" | "output_text") => {
            content_item.get("text").and_then(Value::as_str).is_none()
        }
        Some("input_image") => {
            content_item
                .get("image_url")
                .and_then(Value::as_str)
                .is_none()
                || content_item
                    .get("detail")
                    .is_some_and(|detail| !detail.is_null() && !valid_image_detail(detail))
        }
        _ => true,
    }
}

fn valid_image_detail(detail: &Value) -> bool {
    matches!(detail.as_str(), Some("auto" | "low" | "high" | "original"))
}

/// 判断 agent_message output item 是否缺少官方必需字段。
pub fn websocket_agent_message_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("agent_message")
                && (item.get("author").and_then(Value::as_str).is_none()
                    || item.get("recipient").and_then(Value::as_str).is_none()
                    || item
                        .get("content")
                        .and_then(Value::as_array)
                        .is_none_or(|content| {
                            content
                                .iter()
                                .any(agent_message_content_item_invalid_required_fields)
                        }))
        })
}

fn agent_message_content_item_invalid_required_fields(content_item: &Value) -> bool {
    let Some(content_item) = content_item.as_object() else {
        return true;
    };

    match content_item.get("type").and_then(Value::as_str) {
        Some("input_text") => content_item.get("text").and_then(Value::as_str).is_none(),
        Some("encrypted_content") => content_item
            .get("encrypted_content")
            .and_then(Value::as_str)
            .is_none(),
        _ => true,
    }
}

/// 判断 reasoning output item 是否缺少官方必需字段。
pub fn websocket_reasoning_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("reasoning")
                && (item
                    .get("summary")
                    .and_then(Value::as_array)
                    .is_none_or(|summary| {
                        summary
                            .iter()
                            .any(reasoning_summary_item_invalid_required_fields)
                    })
                    || item.get("id").is_some_and(|id| !id.is_string())
                    || item.get("content").is_some_and(|content| {
                        !content.is_null()
                            && (!content.is_array()
                                || content.as_array().is_some_and(|content| {
                                    content
                                        .iter()
                                        .any(reasoning_content_item_invalid_required_fields)
                                }))
                    })
                    || item
                        .get("encrypted_content")
                        .is_some_and(|encrypted_content| {
                            !encrypted_content.is_null() && !encrypted_content.is_string()
                        }))
        })
}

fn reasoning_summary_item_invalid_required_fields(summary_item: &Value) -> bool {
    let Some(summary_item) = summary_item.as_object() else {
        return true;
    };

    summary_item.get("type").and_then(Value::as_str) != Some("summary_text")
        || summary_item.get("text").and_then(Value::as_str).is_none()
}

fn reasoning_content_item_invalid_required_fields(content_item: &Value) -> bool {
    let Some(content_item) = content_item.as_object() else {
        return true;
    };

    match content_item.get("type").and_then(Value::as_str) {
        Some("reasoning_text" | "text") => {
            content_item.get("text").and_then(Value::as_str).is_none()
        }
        _ => true,
    }
}

/// 判断 function_call output item 是否缺少官方必需字段。
pub fn websocket_function_call_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call")
                && (optional_string_field_invalid(item, "id")
                    || optional_string_field_invalid(item, "namespace")
                    || item.get("name").and_then(Value::as_str).is_none()
                    || item.get("arguments").and_then(Value::as_str).is_none()
                    || item.get("call_id").and_then(Value::as_str).is_none())
        })
}

/// 判断 function_call_output output item 是否缺少官方必需字段。
pub fn websocket_function_call_output_payload_item_event_invalid_required_fields(
    raw: &str,
) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
                && (item.get("call_id").and_then(Value::as_str).is_none()
                    || item
                        .get("output")
                        .is_none_or(function_output_payload_invalid_required_fields))
        })
}

fn function_output_payload_invalid_required_fields(output: &Value) -> bool {
    if output.is_string() {
        return false;
    }

    let Some(output_items) = output.as_array() else {
        return true;
    };

    output_items
        .iter()
        .any(function_output_content_item_invalid_required_fields)
}

fn function_output_content_item_invalid_required_fields(content_item: &Value) -> bool {
    let Some(content_item) = content_item.as_object() else {
        return true;
    };

    match content_item.get("type").and_then(Value::as_str) {
        Some("input_text") => content_item.get("text").and_then(Value::as_str).is_none(),
        Some("input_image") => {
            content_item
                .get("image_url")
                .and_then(Value::as_str)
                .is_none()
                || content_item
                    .get("detail")
                    .is_some_and(|detail| !detail.is_null() && !valid_image_detail(detail))
        }
        Some("encrypted_content") => content_item
            .get("encrypted_content")
            .and_then(Value::as_str)
            .is_none(),
        _ => true,
    }
}

/// 判断 custom_tool_call output item 是否缺少官方必需字段。
pub fn websocket_custom_tool_call_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("custom_tool_call")
                && (optional_string_field_invalid(item, "id")
                    || optional_string_field_invalid(item, "status")
                    || item.get("call_id").and_then(Value::as_str).is_none()
                    || item.get("name").and_then(Value::as_str).is_none()
                    || item.get("input").and_then(Value::as_str).is_none())
        })
}

/// 判断 custom_tool_call_output output item 是否缺少官方必需字段。
pub fn websocket_custom_tool_call_output_payload_item_event_invalid_required_fields(
    raw: &str,
) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("custom_tool_call_output")
                && (optional_string_field_invalid(item, "name")
                    || item.get("call_id").and_then(Value::as_str).is_none()
                    || item
                        .get("output")
                        .is_none_or(function_output_payload_invalid_required_fields))
        })
}

/// 判断 tool_search_call output item 是否缺少官方必需字段。
pub fn websocket_tool_search_call_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("tool_search_call")
                && (optional_string_field_invalid(item, "id")
                    || optional_string_field_invalid(item, "call_id")
                    || optional_string_field_invalid(item, "status")
                    || item.get("execution").and_then(Value::as_str).is_none()
                    || !item.contains_key("arguments"))
        })
}

/// 判断 tool_search_output output item 是否缺少官方必需字段。
pub fn websocket_tool_search_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("tool_search_output")
                && (optional_string_field_invalid(item, "call_id")
                    || item.get("status").and_then(Value::as_str).is_none()
                    || item.get("execution").and_then(Value::as_str).is_none()
                    || item.get("tools").and_then(Value::as_array).is_none())
        })
}

/// 判断 local_shell_call output item 是否缺少官方必需字段。
pub fn websocket_local_shell_call_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("local_shell_call")
                && (optional_string_field_invalid(item, "id")
                    || optional_string_field_invalid(item, "call_id")
                    || !valid_local_shell_status(item.get("status"))
                    || item
                        .get("action")
                        .and_then(Value::as_object)
                        .is_none_or(local_shell_action_invalid_required_fields))
        })
}

fn valid_local_shell_status(status: Option<&Value>) -> bool {
    matches!(
        status.and_then(Value::as_str),
        Some("completed" | "in_progress" | "incomplete")
    )
}

fn local_shell_action_invalid_required_fields(action: &Map<String, Value>) -> bool {
    action.get("type").and_then(Value::as_str) != Some("exec")
        || action
            .get("command")
            .and_then(Value::as_array)
            .is_none_or(|command| command.iter().any(|part| !part.is_string()))
        || optional_u64_field_invalid(action, "timeout_ms")
        || optional_string_field_invalid(action, "working_directory")
        || optional_string_field_invalid(action, "user")
        || optional_string_map_field_invalid(action, "env")
}

fn optional_u64_field_invalid(item: &Map<String, Value>, field: &str) -> bool {
    item.get(field)
        .is_some_and(|value| !value.is_null() && value.as_u64().is_none())
}

fn optional_string_map_field_invalid(item: &Map<String, Value>, field: &str) -> bool {
    item.get(field).is_some_and(|value| {
        !value.is_null()
            && value
                .as_object()
                .is_none_or(|object| object.values().any(|value| !value.is_string()))
    })
}

/// 判断 web_search_call output item 是否缺少官方必需字段。
pub fn websocket_web_search_call_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("web_search_call")
                && (optional_string_field_invalid(item, "id")
                    || optional_string_field_invalid(item, "status")
                    || item
                        .get("action")
                        .is_some_and(web_search_action_invalid_required_fields))
        })
}

fn web_search_action_invalid_required_fields(action: &Value) -> bool {
    if action.is_null() {
        return false;
    }

    let Some(action) = action.as_object() else {
        return true;
    };

    match action.get("type").and_then(Value::as_str) {
        Some("search") => {
            optional_string_field_invalid(action, "query")
                || optional_string_array_field_invalid(action, "queries")
        }
        Some("open_page") => optional_string_field_invalid(action, "url"),
        Some("find_in_page") => {
            optional_string_field_invalid(action, "url")
                || optional_string_field_invalid(action, "pattern")
        }
        Some(_) => false,
        None => true,
    }
}

fn optional_string_array_field_invalid(item: &Map<String, Value>, field: &str) -> bool {
    item.get(field).is_some_and(|value| {
        !value.is_null()
            && value
                .as_array()
                .is_none_or(|items| items.iter().any(|item| !item.is_string()))
    })
}

/// 判断 image_generation_call output item 是否缺少官方必需字段。
pub fn websocket_image_generation_call_output_item_event_invalid_required_fields(
    raw: &str,
) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("image_generation_call")
                && (item.get("id").and_then(Value::as_str).is_none()
                    || item.get("status").and_then(Value::as_str).is_none()
                    || optional_string_field_invalid(item, "revised_prompt")
                    || item.get("result").and_then(Value::as_str).is_none())
        })
}

/// 判断 compaction output item 是否缺少官方必需字段。
pub fn websocket_compaction_output_item_event_invalid_required_fields(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if !is_output_item_event(&value) {
        return false;
    }
    value
        .get("item")
        .and_then(Value::as_object)
        .is_some_and(|item| match item.get("type").and_then(Value::as_str) {
            Some("compaction" | "compaction_summary") => item
                .get("encrypted_content")
                .and_then(Value::as_str)
                .is_none(),
            Some("context_compaction") => optional_string_field_invalid(item, "encrypted_content"),
            _ => false,
        })
}

/// 判断 reasoning summary part added 事件是否缺少 summary_index。
pub fn websocket_reasoning_summary_part_added_missing_summary_index(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    value.get("type").and_then(Value::as_str) == Some("response.reasoning_summary_part.added")
        && value.get("summary_index").and_then(Value::as_i64).is_none()
}
