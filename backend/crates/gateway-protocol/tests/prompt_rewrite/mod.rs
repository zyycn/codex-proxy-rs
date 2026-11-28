//! 各协议的系统指令位置、首条命中与其余 wire 字节保留

use gateway_protocol::prompt_rewrite::plan;
mod wire;

#[test]
fn unmatched_quoted_and_unknown_content_passes_through() {
    for (protocol, body) in [
        (
            "openai",
            r#"{"instructions":"quote: abc","input":[{"role":"system","content":"abc"}]}"#,
        ),
        (
            "openai",
            r#"{"input":[{"type":"function_call","role":"system","content":"abc"}]}"#,
        ),
        (
            "chat_completions",
            r#"{"messages":[{"role":"user","content":"abc"},{"role":"developer","content":"abc"}]}"#,
        ),
        (
            "genai",
            r#"{"contents":[{"role":"user","parts":[{"text":"abc"}]}]}"#,
        ),
        (
            "genai",
            r#"{"systemInstruction":{"parts":[{"text":"untouched"}]},"generateContentRequest":{"systemInstruction":{"parts":[{"text":"abc"}]}}}"#,
        ),
        (
            "anthropic",
            r#"{"system":[{"type":"custom","text":"abc"}]}"#,
        ),
        ("other", r#"{"instructions":"abc"}"#),
    ] {
        let body = serde_json::from_str(body).unwrap();
        assert!(
            plan(protocol, &body, |text| text
                .strip_prefix("abc")
                .map(str::to_owned))
            .is_none()
        );
    }
}
