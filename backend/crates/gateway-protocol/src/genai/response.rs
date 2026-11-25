//! Google 生成候选的完成条件与累计用量

use serde_json::Value;
use std::collections::BTreeMap;

/// 候选的显式结束原因，proto 默认枚举表示仍未结束
fn candidate_finish_reason(candidate: &Value) -> Option<&str> {
    candidate
        .get("finishReason")?
        .as_str()
        .filter(|reason| !matches!(*reason, "" | "FINISH_REASON_UNSPECIFIED"))
}

/// 提示在生成首个候选前已被阻止
fn prompt_is_blocked(value: &Value) -> bool {
    value
        .pointer("/promptFeedback/blockReason")
        .and_then(Value::as_str)
        .is_some_and(|reason| !matches!(reason, "" | "BLOCKED_REASON_UNSPECIFIED"))
}

/// 当前帧携带终止依据时，响应中间件的改写需保留该语义
/// 整条流能否结束仍由所有候选及 EOF 共同决定
pub fn has_finish_signal(value: &Value) -> bool {
    value.get("error").is_some_and(Value::is_object)
        || prompt_is_blocked(value)
        || value
            .get("candidates")
            .and_then(Value::as_array)
            .is_some_and(|candidates| {
                candidates
                    .iter()
                    .any(|candidate| candidate_finish_reason(candidate).is_some())
            })
}

/// 管理探测展示第一候选的可见文本，工具参数和思考内容仍只属于原生响应
pub fn primary_text_parts(value: &Value) -> impl Iterator<Item = &str> {
    value
        .get("candidates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|candidate| {
            candidate
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                == 0
        })
        .filter_map(|candidate| candidate.pointer("/content/parts")?.as_array())
        .flatten()
        .filter(|part| part.get("thought").and_then(Value::as_bool) != Some(true))
        .filter_map(|part| part.get("text")?.as_str())
        .filter(|text| !text.is_empty())
}

/// 单个生成请求共有的候选状态与增量用量，不持有消息、签名或工具参数
#[derive(Default)]
pub struct GenerateContentObservation {
    candidates: BTreeMap<u64, Candidate>,
    blocked: bool,
    usage: UsageMetadata,
}

#[derive(Default)]
struct Candidate {
    tool_call: bool,
    reason: Option<String>,
}

/// Google 的提示阻断和候选完成都能使请求完整结束
#[derive(Debug, PartialEq, Eq)]
pub enum GenerationFinish<'a> {
    Blocked,
    Candidate { reason: &'a str, tool_call: bool },
}

impl GenerateContentObservation {
    /// 更新候选状态，只在本帧提供用量时返回新的累计快照
    pub fn observe(&mut self, value: &Value) -> Option<&UsageMetadata> {
        self.blocked |= prompt_is_blocked(value);
        for candidate in value
            .get("candidates")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            // proto3 允许省略值为零的第一个候选索引
            let index = candidate
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let state = self.candidates.entry(index).or_default();
            state.tool_call |= candidate
                .pointer("/content/parts")
                .and_then(Value::as_array)
                .is_some_and(|parts| {
                    parts
                        .iter()
                        .any(|part| part.get("functionCall").is_some_and(Value::is_object))
                });
            if let Some(reason) = candidate_finish_reason(candidate) {
                state.reason = Some(reason.to_owned());
            }
        }
        let metadata = value.get("usageMetadata")?.as_object()?;
        self.usage.merge(metadata);
        Some(&self.usage)
    }

    /// 仅在完整 JSON 已读完或 SSE 正常 EOF 时调用
    pub fn finish(&self) -> Option<GenerationFinish<'_>> {
        if self.blocked && self.candidates.is_empty() {
            return Some(GenerationFinish::Blocked);
        }
        if self.candidates.values().any(|c| c.reason.is_none()) {
            return None;
        }
        let (_, primary) = self.candidates.first_key_value()?;
        Some(GenerationFinish::Candidate {
            reason: primary.reason.as_deref()?,
            tool_call: primary.tool_call,
        })
    }
}

/// 同一次请求中每个 Google 计数字段均以最近一次显式上报为准
#[derive(Debug, Default)]
pub struct UsageMetadata {
    prompt: Option<u64>,
    tool_input: Option<u64>,
    candidates: Option<u64>,
    prompt_image: Option<u64>,
    tool_image: Option<u64>,
    pub thoughts: Option<u64>,
    pub cached: Option<u64>,
    pub candidate_image: Option<u64>,
    pub total: Option<u64>,
}

impl UsageMetadata {
    fn merge(&mut self, metadata: &serde_json::Map<String, Value>) {
        let count = |key: &str| metadata.get(key).and_then(Value::as_u64);
        let images = |key: &str| {
            metadata
                .get(key)?
                .as_array()?
                .iter()
                .find(|entry| entry.get("modality").and_then(Value::as_str) == Some("IMAGE"))?
                .get("tokenCount")?
                .as_u64()
        };

        self.prompt = count("promptTokenCount").or(self.prompt);
        self.tool_input = count("toolUsePromptTokenCount").or(self.tool_input);
        self.candidates = count("candidatesTokenCount").or(self.candidates);
        self.thoughts = count("thoughtsTokenCount").or(self.thoughts);
        self.cached = count("cachedContentTokenCount").or(self.cached);
        self.total = count("totalTokenCount").or(self.total);
        self.prompt_image = images("promptTokensDetails").or(self.prompt_image);
        self.tool_image = images("toolUsePromptTokensDetails").or(self.tool_image);
        self.candidate_image = images("candidatesTokensDetails").or(self.candidate_image);
    }

    /// prompt 已包含缓存，工具执行返回的输入另列
    pub fn input_tokens(&self) -> Option<u64> {
        combined(self.prompt, self.tool_input)
    }

    /// Google 将思考与候选输出单独计数
    pub fn output_tokens(&self) -> Option<u64> {
        combined(self.candidates, self.thoughts)
    }

    pub fn image_input_tokens(&self) -> Option<u64> {
        combined(self.prompt_image, self.tool_image)
    }
}

fn combined(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => a.checked_add(b),
        _ => a.or(b),
    }
}
