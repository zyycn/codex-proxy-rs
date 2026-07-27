use gateway_protocol::openai::codex_responses_request_semantics;
use serde_json::{Map, Value, json};

#[test]
fn codex_semantics_should_use_protocol_context_before_body_metadata() {
    let body = json!({
        "model": "grok-4.5",
        "reasoning": {"effort": " xhigh "},
        "client_metadata": {
            "x-codex-turn-metadata": "{\"request_kind\":\"review\"}"
        },
        "input": [
            {"type": "message", "role": "user", "content": "history"},
            {"type": "compaction_trigger"}
        ]
    });
    let body = object(body);
    let context = Map::from_iter([(
        "turn_metadata".to_owned(),
        json!("{\"request_kind\":\"compaction\",\"subagent_kind\":\"compact\"}"),
    )]);

    let semantics = codex_responses_request_semantics(&body, &context);

    assert_eq!(semantics.reasoning_effort.as_deref(), Some("xhigh"));
    assert_eq!(semantics.request_kind.as_deref(), Some("compaction"));
    assert_eq!(semantics.subagent_kind.as_deref(), Some("compact"));
    assert_eq!(semantics.reasoning_preset, None);
    assert!(semantics.compact);
}

#[test]
fn codex_semantics_should_detect_ultra_preset_from_proactive_multi_agent() {
    let body = object(json!({
        "model": "gpt-test",
        "reasoning": {"effort": "max"},
        "input": [
            {
                "type": "message",
                "role": "developer",
                "content": [{
                    "type": "input_text",
                    "text": "<multi_agent_mode>Proactive multi-agent delegation is active.</multi_agent_mode>"
                }]
            }
        ]
    }));

    let semantics = codex_responses_request_semantics(&body, &Map::new());

    assert_eq!(semantics.reasoning_effort.as_deref(), Some("max"));
    assert_eq!(semantics.reasoning_preset, Some("ultra"));
    assert!(!semantics.compact);
}

fn object(value: Value) -> Map<String, Value> {
    let Value::Object(object) = value else {
        panic!("test value must be an object");
    };
    object
}
