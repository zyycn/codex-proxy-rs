//! Grok 函数参数中的 custom 输入直接恢复为 Chat 工具增量

use super::*;
use serde_json::json;

pub(super) struct CustomTools {
    names: BTreeSet<String>,
    arguments: BTreeMap<u64, String>,
    bytes: usize,
}

impl CustomTools {
    pub(super) fn new(request: &Map<String, Value>) -> Self {
        let names = request
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|tool| tool.get("type").and_then(Value::as_str) == Some("custom"))
            .filter_map(|tool| {
                tool.pointer("/custom/name")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        Self {
            names,
            arguments: BTreeMap::new(),
            bytes: 0,
        }
    }

    pub(super) fn project(&mut self, chunks: Vec<Value>) -> Result<Vec<Value>, ProviderError> {
        let mut output = Vec::new();
        for mut chunk in chunks {
            if let Some(calls) = chunk
                .pointer_mut("/choices/0/delta/tool_calls")
                .and_then(Value::as_array_mut)
            {
                let mut projected = Vec::with_capacity(calls.len());
                for mut call in calls.drain(..) {
                    let index = call
                        .get("index")
                        .and_then(Value::as_u64)
                        .ok_or_else(invalid)?;
                    let name = call.pointer("/function/name").and_then(Value::as_str);
                    let custom = self.arguments.contains_key(&index)
                        || name.is_some_and(|name| self.names.contains(name));
                    if !custom {
                        projected.push(call);
                        continue;
                    }
                    let arguments = self.arguments.entry(index).or_default();
                    if let Some(delta) = call.pointer("/function/arguments").and_then(Value::as_str)
                    {
                        self.bytes = self
                            .bytes
                            .checked_add(delta.len())
                            .filter(|size| *size <= 4 * 1024 * 1024)
                            .ok_or_else(invalid)?;
                        arguments.push_str(delta);
                    }
                    let Some(name) = name.map(str::to_owned) else {
                        continue;
                    };
                    let object = call.as_object_mut().ok_or_else(invalid)?;
                    object.remove("function");
                    object.insert("type".to_owned(), json!("custom"));
                    object.insert("custom".to_owned(), json!({"name":name,"input":""}));
                    projected.push(call);
                }
                *calls = projected;
            }
            if chunk
                .pointer("/choices/0/finish_reason")
                .is_some_and(Value::is_string)
                && !self.arguments.is_empty()
            {
                let calls = std::mem::take(&mut self.arguments)
                    .into_iter()
                    .map(|(index, arguments)| {
                        let arguments: Value =
                            serde_json::from_str(&arguments).map_err(|_| invalid())?;
                        let input = arguments
                            .get("input")
                            .and_then(Value::as_str)
                            .ok_or_else(invalid)?;
                        Ok(json!({"index":index,"custom":{"input":input}}))
                    })
                    .collect::<Result<Vec<_>, ProviderError>>()?;
                let mut delta = chunk.clone();
                delta["choices"][0]["finish_reason"] = Value::Null;
                delta["choices"][0]["delta"] = json!({"tool_calls":calls});
                output.push(delta);
                self.bytes = 0;
            }
            output.push(chunk);
        }
        Ok(output)
    }
}

fn invalid() -> ProviderError {
    provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
}
