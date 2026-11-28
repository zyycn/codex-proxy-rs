//! 定位生成正文的首条系统指令，同时维护已解析正文和原始 JSON

mod wire;

use serde_json::{Map, Value};

type Path = &'static [&'static str];

/// 已定位的文本字段及替换结果
pub struct PromptEdit {
    content: Path,
    tail: Path,
    text: String,
}

/// 未命中时仅借用系统文本，不复制会话历史
pub fn plan(
    protocol: &str,
    body: &Map<String, Value>,
    replace: impl FnOnce(&str) -> Option<String>,
) -> Option<PromptEdit> {
    use crate::{
        anthropic::ANTHROPIC_PROTOCOL, chat_completions::CHAT_PROTOCOL, genai::GENAI_PROTOCOL,
        openai::OPENAI_PROTOCOL,
    };
    let (content, value, types): (Path, _, &[&str]) = match protocol {
        OPENAI_PROTOCOL | CHAT_PROTOCOL => {
            if let Some(value) = body.get("instructions").filter(|v| !v.is_null()) {
                (&["instructions"], value, &["text", "input_text"])
            } else {
                let path: Path = if protocol == CHAT_PROTOCOL {
                    &["messages", "0", "content"]
                } else {
                    &["input", "0", "content"]
                };
                let first = body.get(path[0])?.get(0)?;
                if !matches!(first.get("role")?.as_str()?, "system" | "developer")
                    || first
                        .get("type")
                        .is_some_and(|v| v.as_str() != Some("message"))
                {
                    return None;
                }
                (path, first.get("content")?, &["text", "input_text"])
            }
        }
        ANTHROPIC_PROTOCOL => (&["system"], body.get("system")?, &["text"]),
        GENAI_PROTOCOL => {
            let path: Path = if body.contains_key("systemInstruction") {
                &["systemInstruction", "parts", "0", "text"]
            } else {
                &["system_instruction", "parts", "0", "text"]
            };
            let value = body.get(path[0])?.get("parts")?.get(0)?.get("text")?;
            return Some(PromptEdit {
                content: path,
                tail: &[],
                text: replace(value.as_str()?)?,
            });
        }
        _ => return None,
    };
    let (tail, text): (Path, _) = if let Some(parts) = value.as_array() {
        let first = parts.first()?;
        if !types.contains(&first.get("type")?.as_str()?) {
            return None;
        }
        (&["0", "text"], first.get("text")?.as_str()?)
    } else {
        (&[], value.as_str()?)
    };
    Some(PromptEdit {
        content,
        tail,
        text: replace(text)?,
    })
}

impl PromptEdit {
    /// 将编辑应用于定位时使用的正文
    pub fn apply(self, body: &mut Map<String, Value>) {
        let Some(mut field) = body.get_mut(self.content[0]) else {
            return;
        };
        for &key in self.content[1..].iter().chain(self.tail) {
            let Some(child) = (if key == "0" {
                field.get_mut(0)
            } else {
                field.get_mut(key)
            }) else {
                return;
            };
            field = child;
        }
        *field = Value::String(self.text);
    }

    /// 只编码目标字符串，保留其他字段的原始字节
    pub fn wire(&self, body: &[u8]) -> Option<Vec<u8>> {
        let mut raw = serde_json::from_slice(body).ok()?;
        for &key in self.content.iter().chain(self.tail) {
            raw = if key == "0" {
                wire::first(raw)?
            } else {
                wire::field(raw, key)?
            };
        }
        let start = raw.get().as_ptr() as usize - body.as_ptr() as usize;
        let mut out = Vec::with_capacity(body.len() - raw.get().len() + self.text.len() + 2);
        out.extend_from_slice(&body[..start]);
        serde_json::to_writer(&mut out, &self.text).ok()?;
        out.extend_from_slice(&body[start + raw.get().len()..]);
        Some(out)
    }
}
