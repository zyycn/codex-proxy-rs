//! Chat 补全编排。

use crate::protocol::{
    codex::responses::CodexResponsesRequest,
    openai::chat::{translate_chat_to_codex, ChatCompletionRequest, ChatTranslationError},
};

/// 将 Chat 请求准备为上游 Responses 请求。
pub fn prepare_chat_request(
    request: ChatCompletionRequest,
) -> Result<CodexResponsesRequest, ChatTranslationError> {
    translate_chat_to_codex(request)
}
