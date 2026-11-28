//! 各协议的系统指令位置、首条命中与其余 wire 字节保留

use gateway_protocol::prompt_rewrite::plan;
use serde_json::Map;

#[test]
fn rewrites_only_the_authoritative_text_and_preserves_wire_bytes() {
    for (protocol, instruction) in [
        (
            "chat_completions",
            r#""messages":[{"role":"system","content":"First\n规则"},{"role":"user","content":"First\n规则"}]"#,
        ),
        (
            "openai",
            r#""instructions":"First\n规则","input":[{"role":"developer","content":"First\n规则"}]"#,
        ),
        (
            "openai",
            r#""input":[{"type":"message","role":"developer","content":[{"type":"input_text","text":"First\n规则","extra":7}]}]"#,
        ),
        ("anthropic", r#""system":"First\n规则""#),
        (
            "anthropic",
            r#""system":[{"type":"text","text":"First\n规则","cache_control":{"type":"ephemeral"}},{"type":"text","text":"First\n规则"}]"#,
        ),
        (
            "genai",
            r#""systemInstruction":{"parts":[{"text":"First\n规则"},{"text":"First\n规则"}]}"#,
        ),
        (
            "genai",
            r#""system_instruction":{"parts":[{"text":"First\n规则"}]}"#,
        ),
    ] {
        let body = format!(
            r#"{{"unknown":1e+08, {instruction}, "unknown":12345678901234567890123456789}}"#
        );
        let parsed: Map<_, _> = serde_json::from_str(&body).unwrap();
        let edit = plan(protocol, &parsed, |text| {
            text.strip_prefix("First\n")
                .map(|suffix| format!("Changed\n{suffix}"))
        })
        .unwrap();
        let wire = edit.wire(body.as_bytes()).unwrap();
        let mut edited = parsed;
        edit.apply(&mut edited);
        assert_eq!(edited, serde_json::from_slice::<Map<_, _>>(&wire).unwrap());
        assert_eq!(wire, body.replacen("First", "Changed", 1).as_bytes());
    }
}
