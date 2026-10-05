//! 验证 Codex 会话身份优先级、轮次元数据与多代理模式解析

use gateway_protocol::openai::{
    codex_responses_request_semantics, codex_session_id, codex_thread_id,
};
use serde_json::{Map, Value, json};

#[test]
fn codex_session_should_prefer_the_connection_identity_over_body_metadata() {
    let body = object(
        json!({"session_id":"body-session", "client_metadata":{"session_id":"metadata-session"}}),
    );
    let context = object(
        json!({"session_id":" root-session ", "turn_metadata":"{\"session_id\":\"turn-session\"}"}),
    );
    assert_eq!(
        codex_session_id(&body, &context).as_deref(),
        Some("root-session")
    );
}

#[test]
fn codex_session_should_accept_explicit_metadata_and_ignore_unrelated_ids() {
    for (body, context) in [
        (json!({"session_id":"root"}), json!({})),
        (json!({"client_metadata":{"session_id":"root"}}), json!({})),
        (
            json!({"client_metadata":{"x-codex-turn-metadata":"{\"session_id\":\"root\"}"}}),
            json!({}),
        ),
        (
            json!({}),
            json!({"turn_metadata":"{\"session_id\":\"root\"}"}),
        ),
        (json!({"context":{"session_id":"root"}}), json!({})),
    ] {
        assert_eq!(
            codex_session_id(&object(body), &object(context)).as_deref(),
            Some("root")
        );
    }
    for body in [
        json!({"session_id":" ", "thread_id":"child", "turn_id":"turn", "id":"request"}),
        json!({"client_metadata":{"x-codex-turn-metadata":"invalid JSON"}}),
    ] {
        assert!(codex_session_id(&object(body), &Map::new()).is_none());
    }
}

#[test]
fn codex_thread_should_use_explicit_identity_and_never_guess_from_other_ids() {
    for (body, context, expected) in [
        (
            json!({"thread_id":"body", "client_metadata":{"thread_id":"metadata"}}),
            json!({"thread_id":" child "}),
            Some("child"),
        ),
        (
            json!({"thread_id":"body", "client_metadata":{"thread_id":"metadata"}}),
            json!({}),
            Some("body"),
        ),
        (
            json!({"client_metadata":{"thread_id":"metadata"}}),
            json!({}),
            Some("metadata"),
        ),
        (
            json!({"client_metadata":{"x-codex-turn-metadata":"{\"thread_id\":\"metadata-thread\"}"}}),
            json!({}),
            Some("metadata-thread"),
        ),
        (
            json!({"turnMetadata":"{\"thread_id\":\"body-turn\"}"}),
            json!({"turn_metadata":"{\"thread_id\":\"header-turn\"}"}),
            Some("header-turn"),
        ),
        (
            json!({"context":{"thread_id":"context-thread"}}),
            json!({}),
            Some("context-thread"),
        ),
        (
            json!({"thread_id":" ", "session_id":"root", "turn_id":"turn", "agent_name":"agent"}),
            json!({}),
            None,
        ),
        (
            json!({"client_metadata":{"x-codex-turn-metadata":"invalid JSON"}}),
            json!({}),
            None,
        ),
    ] {
        assert_eq!(
            codex_thread_id(&object(body), &object(context)).as_deref(),
            expected
        );
    }
}

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
