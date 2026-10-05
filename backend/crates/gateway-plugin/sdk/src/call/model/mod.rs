//! 宿主模型调用的流事件合同

use serde::{Deserialize, Serialize};

pub(in crate::call) mod codec;
pub mod facts;
pub use codec::{ExecutionEncodingError, MAX_EXECUTION_PAYLOAD_BYTES};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Text,
    Reasoning,
    ToolCall,
    Image,
    Audio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCall,
    ContentFilter,
    Other,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub image_input_tokens: Option<u64>,
    pub image_output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

/// 每个流分块是一个完整封套；同一 wire 的事实必须合并，避免重复交付客户端
/// 载荷及私有状态不实现 Debug，且不能用拆分封套绕过宿主大小和序列检查
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionEvent {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<CanonicalEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire: Option<WireEvent>,
    /// 宿主确认的来源事实；回传快照不会触发计量、续接或发送状态更新
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<Box<facts::ExecutionFacts>>,
}

impl ExecutionEvent {
    /// 编码为带长度与版本的事件载荷；原始 wire/错误正文不经过 JSON 数组或 base64
    ///
    /// # Errors
    /// 总长度超过事件上限或元数据无法序列化时返回错误
    pub fn encode(self) -> Result<Vec<u8>, ExecutionEncodingError> {
        codec::encode_event(self)
    }

    /// 从完整的执行流载荷解码，先验证各段长度再解析元数据
    ///
    /// # Errors
    /// 版本、长度、元数据或二进制段声明不一致时返回错误
    pub fn decode(bytes: &[u8]) -> Result<Self, ExecutionEncodingError> {
        codec::decode_event(bytes)
    }

    #[must_use]
    pub fn canonical(fact: CanonicalEvent) -> Self {
        Self {
            facts: vec![fact],
            ..Self::default()
        }
    }

    #[must_use]
    pub fn wire(wire: WireEvent) -> Self {
        Self {
            wire: Some(wire),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_fact(mut self, fact: CanonicalEvent) -> Self {
        self.facts.push(fact);
        self
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CanonicalEvent {
    Started {
        id: String,
        model: Option<String>,
    },
    ContentAdded {
        index: u32,
        kind: ContentKind,
    },
    TextDelta {
        index: u32,
        text: String,
    },
    ReasoningDelta {
        index: u32,
        text: String,
    },
    ToolCallDelta {
        index: u32,
        id: String,
        name: Option<String>,
        arguments: String,
    },
    Usage {
        usage: Usage,
    },
    Completed {
        id: String,
        model: Option<String>,
        reason: FinishReason,
    },
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireEvent {
    pub protocol: String,
    pub payload: WirePayload,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WirePayload {
    Json {
        event: Option<String>,
        data: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry: Option<u64>,
        /// 一个完整 SSE 帧的原字节；存在时必须与解析后的 data 和元数据一致
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_sse: Option<Vec<u8>>,
    },
    /// 注释、非 JSON data 等原生 SSE 表达，不能夹带第二个帧
    RawSse { frame: Vec<u8> },
    /// 一个完整的非流式 JSON 响应正文，保留空白与字段顺序
    RawJson { body: Vec<u8> },
    /// 非流式 HTTP 响应正文的一个有序片段
    /// 客户端 adapter 必须同时限制单片与聚合后的总字节数
    RawBody { body: Vec<u8> },
}
