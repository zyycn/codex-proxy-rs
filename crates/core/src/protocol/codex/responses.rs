use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Codex Responses 上游请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexResponsesRequest {
    /// 模型名。
    pub model: String,
    /// 指令文本。
    pub instructions: String,
    /// 输入消息与结构化条目。
    pub input: Vec<Value>,
    /// 是否流式返回。
    pub stream: bool,
    /// 是否要求上游存储响应。
    pub store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// reasoning 配置。
    pub reasoning: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 工具定义。
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 工具选择策略。
    pub tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 是否允许并行工具调用。
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 输出文本格式配置。
    pub text: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 图片生成开关。
    pub generate: Option<bool>,
    #[serde(skip)]
    /// tuple schema 原始定义，仅供响应重构时使用。
    pub tuple_schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// service tier。
    pub service_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 用于显式续链的前一个 response ID。
    pub previous_response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 提示缓存键。
    pub prompt_cache_key: Option<String>,
    #[serde(skip)]
    /// 是否由客户端显式提供了 prompt cache key。
    pub explicit_prompt_cache_key: bool,
    #[serde(skip)]
    /// 客户端会话 ID。
    pub client_conversation_id: Option<String>,
    #[serde(skip)]
    /// 变体身份键。
    pub variant_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// include 列表。
    pub include: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 传给上游的 client metadata。
    pub client_metadata: Option<Value>,
    #[serde(skip)]
    /// 是否偏好 WebSocket 传输。
    pub use_websocket: bool,
    #[serde(skip)]
    /// 是否强制 HTTP SSE。
    pub force_http_sse: bool,
    #[serde(skip)]
    /// turn state 透传头。
    pub turn_state: Option<String>,
    #[serde(skip)]
    /// turn metadata 透传头。
    pub turn_metadata: Option<String>,
    #[serde(skip)]
    /// beta features 透传头。
    pub beta_features: Option<String>,
    #[serde(skip)]
    /// 客户端版本头。
    pub version: Option<String>,
    #[serde(skip)]
    /// timing metrics 透传头。
    pub include_timing_metrics: Option<String>,
    #[serde(skip)]
    /// codex window id。
    pub codex_window_id: Option<String>,
    #[serde(skip)]
    /// 父线程 ID。
    pub parent_thread_id: Option<String>,
}

impl CodexResponsesRequest {
    /// 构造默认的 HTTP SSE 请求。
    pub fn new_http_sse(
        model: impl Into<String>,
        instructions: impl Into<String>,
        input: Vec<Value>,
    ) -> Self {
        Self {
            model: model.into(),
            instructions: instructions.into(),
            input,
            stream: true,
            store: false,
            reasoning: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            text: None,
            generate: None,
            tuple_schema: None,
            service_tier: None,
            previous_response_id: None,
            prompt_cache_key: None,
            explicit_prompt_cache_key: false,
            client_conversation_id: None,
            variant_identity: None,
            include: None,
            client_metadata: None,
            use_websocket: false,
            force_http_sse: false,
            turn_state: None,
            turn_metadata: None,
            beta_features: None,
            version: None,
            include_timing_metrics: None,
            codex_window_id: None,
            parent_thread_id: None,
        }
    }

    /// 判断请求是否声明了图片生成工具。
    pub fn expects_image_generation(&self) -> bool {
        self.tools.as_deref().is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool.get("type").and_then(Value::as_str) == Some("image_generation"))
        })
    }
}

/// Codex compact 端点请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexCompactRequest {
    /// 模型名。
    pub model: String,
    /// 输入消息与结构化条目。
    pub input: Vec<Value>,
    /// 指令文本。
    pub instructions: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 工具定义。
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 是否允许并行工具调用。
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// reasoning 配置。
    pub reasoning: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 输出文本格式配置。
    pub text: Option<Value>,
}
