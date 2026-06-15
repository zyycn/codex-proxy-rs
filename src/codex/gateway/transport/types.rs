use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexResponsesRequest {
    pub model: String,
    pub instructions: String,
    pub input: Vec<Value>,
    pub stream: bool,
    pub store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<Value>,
    #[serde(skip)]
    pub tuple_schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(skip)]
    pub explicit_prompt_cache_key: bool,
    #[serde(skip)]
    pub client_conversation_id: Option<String>,
    #[serde(skip)]
    pub variant_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_metadata: Option<Value>,
    #[serde(skip)]
    pub use_websocket: bool,
    #[serde(skip)]
    pub force_http_sse: bool,
    #[serde(skip)]
    pub turn_state: Option<String>,
    #[serde(skip)]
    pub turn_metadata: Option<String>,
    #[serde(skip)]
    pub beta_features: Option<String>,
    #[serde(skip)]
    pub version: Option<String>,
    #[serde(skip)]
    pub include_timing_metrics: Option<String>,
    #[serde(skip)]
    pub codex_window_id: Option<String>,
    #[serde(skip)]
    pub parent_thread_id: Option<String>,
}

impl CodexResponsesRequest {
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

    pub fn expects_image_generation(&self) -> bool {
        self.tools.as_deref().is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool.get("type").and_then(Value::as_str) == Some("image_generation"))
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexCompactRequest {
    pub model: String,
    pub input: Vec<Value>,
    pub instructions: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<Value>,
}
