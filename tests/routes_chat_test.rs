use codex_proxy_rs::translation::openai_to_codex::{
    translate_chat_to_codex, ChatCompletionRequest, ChatMessage,
};

#[test]
fn chat_completion_translates_to_codex_response_request() {
    let req = ChatCompletionRequest {
        model: "gpt-5.5".to_string(),
        stream: true,
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        }],
    };

    let codex = translate_chat_to_codex(req).unwrap();
    assert_eq!(codex.model, "gpt-5.5");
    assert!(codex.stream);
    assert_eq!(codex.input[0]["role"], "user");
}
