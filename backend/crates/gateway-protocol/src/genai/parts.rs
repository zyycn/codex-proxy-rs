//! GenAI 轮次中的签名和多模态函数返回
use crate::anthropic::{GENAI_CARRIER as SIGNATURE_PREFIX, TranslationError};
use serde_json::{Map, Value, json};

pub(super) fn invalid() -> TranslationError {
    TranslationError::InvalidRequest(
        "The request contains content unsupported by the GenAI adapter.",
    )
}

// 工具截图可能很大，组装轮次时直接移入已编码的媒体数据
pub(super) fn content(role: &str, parts: Vec<Value>) -> Value {
    Value::Object(Map::from_iter([
        ("role".into(), json!(role)),
        ("parts".into(), Value::Array(parts)),
    ]))
}

#[derive(Default)]
pub(super) struct TurnParts {
    parts: Vec<Value>,
    pending_signature: Option<Value>,
}

impl TurnParts {
    pub(super) fn thinking(&mut self, block: &Value) -> Result<(), TranslationError> {
        let Some(raw) = block["signature"]
            .as_str()
            .and_then(|s| s.strip_prefix(SIGNATURE_PREFIX))
        else {
            // 只回放由本协议返回的思考签名
            return Ok(());
        };
        let carrier: Value = serde_json::from_str(raw).map_err(|_| invalid())?;
        let signature = carrier["signature"].as_str().ok_or_else(invalid)?;
        match carrier["target"].as_str() {
            Some("previous") => {
                self.parts.last_mut().ok_or_else(invalid)?["thoughtSignature"] = json!(signature)
            }
            Some("next") => self.pending_signature = Some(json!(signature)),
            Some("thinking") => self.push(
                json!({"text":block["thinking"],"thought":true,"thoughtSignature":signature}),
            ),
            _ => return Err(invalid()),
        }
        Ok(())
    }
    pub(super) fn push(&mut self, mut part: Value) {
        if let Some(signature) = self.pending_signature.take() {
            part["thoughtSignature"] = signature;
        }
        // Google 为外部历史提供的兜底值；真实签名优先，Chat 等无签名合同无需服务端缓存
        // https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/thinking/thought-signatures
        if part.get("functionCall").is_some() && part.get("thoughtSignature").is_none() {
            part["thoughtSignature"] = json!("skip_thought_signature_validator");
        }
        self.parts.push(part);
    }
    pub(super) fn finish(mut self) -> Result<Vec<Value>, TranslationError> {
        if self.pending_signature.is_some() {
            return Err(invalid());
        }
        self.parts
            .retain(|part| part["text"] != "" || part.get("thoughtSignature").is_some());
        Ok(self.parts)
    }
}

pub(super) fn function_response(
    id: &str,
    name: &str,
    error: bool,
    output: &Value,
    parse_part: impl Fn(&Value) -> Result<Value, TranslationError>,
) -> Result<Value, TranslationError> {
    let mut result = json!({"id":id,"name":name});
    let key = if error { "error" } else { "output" };
    let value = if let Some(blocks) = output.as_array() {
        let mut texts = Vec::new();
        let mut media = Vec::new();
        for block in blocks {
            let part = parse_part(block)?;
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                texts.push(text.to_owned());
            } else {
                media.push(part);
            }
        }
        if !media.is_empty() {
            result["parts"] = Value::Array(media);
        }
        Value::String(texts.join("\n"))
    } else {
        output.clone()
    };
    result["response"] = json!({(key):value});
    Ok(Value::Object(Map::from_iter([(
        "functionResponse".into(),
        result,
    )])))
}
