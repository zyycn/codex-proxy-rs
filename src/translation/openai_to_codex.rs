use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    codex::types::CodexResponsesRequest,
    error::{AppError, AppResult},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    #[serde(default)]
    pub stream: bool,
    pub messages: Vec<ChatMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

pub fn translate_chat_to_codex(req: ChatCompletionRequest) -> AppResult<CodexResponsesRequest> {
    if req.messages.is_empty() {
        return Err(AppError::BadRequest(
            "messages must not be empty".to_string(),
        ));
    }
    let input = req
        .messages
        .into_iter()
        .map(|message| json!({ "role": message.role, "content": message.content }))
        .collect();
    Ok(CodexResponsesRequest {
        model: req.model,
        instructions: String::new(),
        input,
        stream: req.stream,
        store: false,
        reasoning: None,
        tools: None,
        previous_response_id: None,
        use_websocket: false,
    })
}
