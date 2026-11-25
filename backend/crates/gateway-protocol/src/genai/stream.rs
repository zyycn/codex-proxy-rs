//! GenAI 增量帧转为 Messages 事件，终态在上游 EOF 后输出

use super::{GenerateContentObservation, GenerationFinish};
use crate::anthropic::GENAI_CARRIER as SIGNATURE_PREFIX;
use crate::anthropic::TranslationError;
use serde_json::{Value, json};

/// 每条上游流独占转换状态；同一文本块跨帧延续，签名和尾部用量不会提前丢弃
#[derive(Default)]
pub struct GenAiToMessagesStream {
    observation: GenerateContentObservation,
    model: String,
    started: bool,
    finished: bool,
    index: usize,
    usage: Value,
    active_thinking: Option<bool>,
    pending_empty: Option<bool>,
}

impl GenAiToMessagesStream {
    /// 上游首帧未附模型版本时使用请求模型，避免交付 null 模型字段
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Self::default()
        }
    }

    /// 消费已解开 Cloud Code 外层的原生响应
    pub fn push(&mut self, value: &Value) -> Result<Vec<Value>, TranslationError> {
        if self.finished {
            return Err(TranslationError::InvalidEvent(
                "The GenAI stream has already ended.",
            ));
        }
        let mut events = Vec::new();
        if !self.started {
            self.started = true;
            let id = value["responseId"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("msg_{}", uuid::Uuid::new_v4()));
            events.push(json!({"type":"message_start","message":{"id":id,"type":"message","role":"assistant","model":value["modelVersion"].as_str().unwrap_or(&self.model),"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}}));
        }
        if let Some(usage) = self.observation.observe(value) {
            self.usage = json!({"input_tokens":usage.input_tokens().unwrap_or_default().saturating_sub(usage.cached.unwrap_or_default()),"cache_read_input_tokens":usage.cached.unwrap_or_default(),"output_tokens":usage.output_tokens().unwrap_or_default()});
        }
        for candidate in value["candidates"].as_array().into_iter().flatten() {
            if candidate["index"].as_u64().unwrap_or_default() != 0 {
                return Err(TranslationError::InvalidEvent(
                    "Multiple GenAI candidates cannot be translated to a single message.",
                ));
            }
            for part in candidate["content"]["parts"]
                .as_array()
                .into_iter()
                .flatten()
            {
                self.part(part, &mut events)?;
            }
        }
        Ok(events)
    }

    fn part(&mut self, part: &Value, events: &mut Vec<Value>) -> Result<(), TranslationError> {
        let signature = part.get("thoughtSignature");
        if let Some(call) = part.get("functionCall") {
            self.pending_empty = None;
            self.close(None, events);
            if let Some(signature) = signature {
                self.carrier(signature, "next", events);
            }
            let id = call["id"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4()));
            let index = self.index;
            self.index += 1;
            events.push(json!({"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":id,"name":call["name"],"input":{}}}));
            events.push(json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":call.get("args").unwrap_or(&json!({})).to_string()}}));
            events.push(json!({"type":"content_block_stop","index":index}));
        } else if let Some(text) = part.get("text").and_then(Value::as_str) {
            let thinking = part["thought"] == true;
            if text.is_empty() && signature.is_none() {
                // 空占位仅在后续携带签名时保留，避免 Claude 历史回放被空 text 拒绝
                if self.active_thinking.is_none() {
                    self.pending_empty = Some(thinking);
                }
                return Ok(());
            }
            self.pending_empty = None;
            self.text(thinking, text, events);
            if let Some(signature) = signature {
                self.signature(signature, events);
            }
        } else if let Some(signature) = signature {
            if self.active_thinking.is_none()
                && let Some(thinking) = self.pending_empty.take()
            {
                self.text(thinking, "", events);
            }
            self.signature(signature, events);
        } else {
            return Err(TranslationError::InvalidEvent(
                "The GenAI response contains unsupported content.",
            ));
        }
        Ok(())
    }

    fn text(&mut self, thinking: bool, text: &str, events: &mut Vec<Value>) {
        if self
            .active_thinking
            .is_some_and(|active| active != thinking)
        {
            self.close(None, events);
        }
        if self.active_thinking.is_none() {
            let block = if thinking {
                json!({"type":"thinking","thinking":"","signature":""})
            } else {
                json!({"type":"text","text":""})
            };
            events.push(
                json!({"type":"content_block_start","index":self.index,"content_block":block}),
            );
            self.index += 1;
            self.active_thinking = Some(thinking);
        }
        if !text.is_empty() {
            let mut delta = json!({"type":if thinking {"thinking_delta"} else {"text_delta"}});
            delta[if thinking { "thinking" } else { "text" }] = json!(text);
            events.push(json!({"type":"content_block_delta","index":self.index-1,"delta":delta}));
        }
    }

    fn signature(&mut self, signature: &Value, events: &mut Vec<Value>) {
        match self.active_thinking {
            Some(true) => self.close(Some(signature), events),
            // 普通文本签名不强制回传；追加空思考载体或拆分正文会使 Claude Code
            // 的最终 result 为空或只有末段；保持正文连续，原生入口仍完整透传签名
            Some(false) => {}
            None => self.carrier(signature, "previous", events),
        }
    }

    fn close(&mut self, signature: Option<&Value>, events: &mut Vec<Value>) {
        let Some(thinking) = self.active_thinking.take() else {
            return;
        };
        let index = self.index - 1;
        if thinking {
            events.push(json!({"type":"content_block_delta","index":index,"delta":{"type":"signature_delta","signature":carrier(signature.unwrap_or(&Value::Null),"thinking")}}));
        }
        events.push(json!({"type":"content_block_stop","index":index}));
    }

    /// 只有上游正常 EOF 且存在完成信号时才发出完成事件
    pub fn finish(&mut self) -> Result<Vec<Value>, TranslationError> {
        if self.finished {
            return Err(TranslationError::IncompleteStream);
        }
        let reason = match self
            .observation
            .finish()
            .ok_or(TranslationError::IncompleteStream)?
        {
            GenerationFinish::Blocked => "refusal",
            GenerationFinish::Candidate {
                reason: "MAX_TOKENS",
                ..
            } => "max_tokens",
            GenerationFinish::Candidate {
                reason: "STOP" | "OTHER",
                tool_call: true,
            } => "tool_use",
            GenerationFinish::Candidate { reason: "STOP", .. } => "end_turn",
            GenerationFinish::Candidate { .. } => "refusal",
        };
        self.finished = true;
        let mut events = Vec::new();
        self.close(None, &mut events);
        events.push(json!({"type":"message_delta","delta":{"stop_reason":reason,"stop_sequence":null},"usage":self.usage}));
        events.push(json!({"type":"message_stop"}));
        Ok(events)
    }

    fn carrier(&mut self, signature: &Value, target: &str, events: &mut Vec<Value>) {
        let index = self.index;
        self.index += 1;
        events.push(json!({"type":"content_block_start","index":index,"content_block":{"type":"thinking","thinking":"","signature":""}}));
        events.push(json!({"type":"content_block_delta","index":index,"delta":{"type":"signature_delta","signature":carrier(signature,target)}}));
        events.push(json!({"type":"content_block_stop","index":index}));
    }
}

fn carrier(signature: &Value, target: &str) -> String {
    format!(
        "{SIGNATURE_PREFIX}{}",
        json!({"signature":signature,"target":target})
    )
}
