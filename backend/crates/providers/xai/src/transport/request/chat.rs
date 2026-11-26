//! Chat 直接生成 Grok 实际使用的 Responses wire，不经过 Harness 归一化

use super::*;
use gateway_protocol::chat_completions::{CHAT_PROTOCOL, custom_tool_schema, responses};

impl GrokResponsesRequest {
    pub(crate) fn from_chat(
        request: &GenerateRequest,
        upstream_model: &str,
        client: &ClientApiKeyId,
    ) -> Result<Self, GrokRequestEncodeError> {
        if request.protocol_payload().protocol() != CHAT_PROTOCOL
            || request.source_protocol() != CHAT_PROTOCOL
        {
            return Err(GrokRequestEncodeError::InvalidProtocolPayload);
        }
        let mut body = responses::convert_request(request.protocol_payload().body())?;
        // 上游只接受函数工具，custom 的字符串输入直接编码到此目标的参数合同
        if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
            for (index, tool) in tools.iter_mut().enumerate() {
                if tool.get("type").and_then(Value::as_str) != Some("custom") {
                    continue;
                }
                let source = &request.protocol_payload().body()["tools"][index];
                let path = if source.get("custom").is_some_and(|value| !value.is_null()) {
                    format!("tools[{index}].custom.format")
                } else {
                    format!("tools[{index}].format")
                };
                let schema = custom_tool_schema(tool.get("format"), &path)?;
                let mut function = Map::from_iter([
                    ("type".to_owned(), json!("function")),
                    ("name".to_owned(), tool["name"].clone()),
                    ("parameters".to_owned(), schema),
                ]);
                if let Some(description) = tool.get("description") {
                    function.insert("description".to_owned(), description.clone());
                }
                *tool = Value::Object(function);
            }
        }
        if let Some(choice) = body.get_mut("tool_choice") {
            lower_custom_choice(choice);
        }
        if let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) {
            for item in input {
                let Some(item) = item.as_object_mut() else {
                    continue;
                };
                match item.get("type").and_then(Value::as_str) {
                    Some("custom_tool_call") => {
                        let input = item
                            .remove("input")
                            .ok_or(GrokRequestEncodeError::InvalidProtocolPayload)?;
                        item.insert("type".to_owned(), json!("function_call"));
                        item.insert(
                            "arguments".to_owned(),
                            json!(json!({"input":input}).to_string()),
                        );
                    }
                    Some("custom_tool_call_output") => {
                        item.insert("type".to_owned(), json!("function_call_output"));
                    }
                    _ => {}
                }
            }
        }
        let seed = request.prompt_cache_key();
        let identity = resolve_session_identity(client.as_str(), upstream_model, seed, &body);
        sanitize_account_identity(&mut body);
        let (session_id, affinity) =
            identity.map_or((None, None), |(id, affinity)| (Some(id), Some(affinity)));
        if let Some(session) = &session_id {
            body.insert("prompt_cache_key".to_owned(), json!(session));
        }
        body.insert("model".to_owned(), json!(upstream_model));
        body.insert("stream".to_owned(), json!(true));
        Ok(Self {
            body,
            session_id,
            affinity,
        })
    }
}

// 受限工具列表中的引用必须与本次直接生成的函数声明保持一致
fn lower_custom_choice(choice: &mut Value) {
    if choice.get("type").and_then(Value::as_str) == Some("custom") {
        choice["type"] = json!("function");
    }
    if let Some(tools) = choice.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools {
            lower_custom_choice(tool);
        }
    }
}
