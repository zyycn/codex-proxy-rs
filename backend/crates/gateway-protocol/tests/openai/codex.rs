//! 验证 Codex 会话身份优先级、轮次元数据与多代理模式解析

use gateway_protocol::openai::{
    codex_responses_request_semantics, codex_responses_request_semantics_with_turn_metadata,
    codex_session_id, codex_thread_id,
};
use serde_json::{Map, Value, json};

#[test]
fn reasoning_effort_should_observe_unsigned_numbers_without_changing_the_body() {
    for (effort, expected) in [
        (json!(0), Some("0")),
        (json!(64), Some("64")),
        (json!(u64::MAX), Some("18446744073709551615")),
        (json!(" high "), Some("high")),
        (json!(-1), None),
        (json!(1.5), None),
        (json!(true), None),
        (json!(null), None),
    ] {
        let body = object(json!({"reasoning":{"effort":effort}}));
        let semantics = codex_responses_request_semantics(&body, &Map::new());
        assert_eq!(semantics.reasoning_effort.as_deref(), expected);
        assert_eq!(body["reasoning"]["effort"], effort);
    }
}

#[test]
fn reasoning_effort_should_follow_the_latest_configuration_update() {
    for (input, previous_id, inherited, expected) in [
        (json!([]), None, None, "medium"),
        (json!([]), Some("resp_previous"), Some("high"), "high"),
        (json!([]), None, Some("high"), "medium"),
        (
            json!([
                {"type":"configuration_update","reasoning":{"effort":"high"}},
                {"type":"message","role":"user","content":"first turn"},
                {"type":"configuration_update","reasoning":{"effort":"low"}},
                {"type":"message","role":"user","content":"next turn"}
            ]),
            Some("resp_previous"),
            Some("max"),
            "low",
        ),
        (
            json!([{"type":"configuration_update","reasoning":{"effort":64}}]),
            None,
            None,
            "64",
        ),
        (
            json!([{"type":"configuration_update","reasoning":{"effort":" future-value "}}]),
            None,
            None,
            "future-value",
        ),
        (
            json!([{"type":"message","role":"user","reasoning":{"effort":"high"},
                "content":"configuration_update: high"}]),
            None,
            None,
            "medium",
        ),
    ] {
        let body = object(json!({
            "reasoning":{"effort":"medium"},
            "input":input,
            "previous_response_id":previous_id,
        }));
        let original = body.clone();
        let semantics =
            codex_responses_request_semantics_with_turn_metadata(&body, None, inherited);
        assert_eq!(semantics.reasoning_effort.as_deref(), Some(expected));
        assert_eq!(body, original);
    }
}

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
    let mut body = object(json!({
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

    for (role, message_type, expected) in [
        ("system", Some("message"), Some("ultra")),
        ("system", None, None),
        ("system", Some("function_call"), None),
        ("user", Some("message"), None),
        (" system ", Some("message"), None),
    ] {
        let mut shaped = body.clone();
        let item = shaped["input"][0].as_object_mut().unwrap();
        item.insert("role".to_owned(), json!(role));
        if let Some(message_type) = message_type {
            item.insert("type".to_owned(), json!(message_type));
        } else {
            item.remove("type");
        }
        assert_eq!(
            codex_responses_request_semantics(&shaped, &Map::new()).reasoning_preset,
            expected
        );
    }

    for (effort, expected_preset) in [("high", None), ("max", Some("ultra"))] {
        body["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"configuration_update","reasoning":{"effort":effort}}));
        let semantics = codex_responses_request_semantics(&body, &Map::new());
        assert_eq!(semantics.reasoning_effort.as_deref(), Some(effort));
        assert_eq!(semantics.reasoning_preset, expected_preset);
    }
}

#[test]
fn empty_authoritative_turn_metadata_does_not_fall_back_to_body_aliases() {
    for (context, body) in [
        (
            json!({"turn_metadata":""}),
            json!({"turnMetadata":"{\"request_kind\":\"review\"}"}),
        ),
        (
            json!({"turn_metadata":" "}),
            json!({"turnMetadata":"{\"request_kind\":\"review\"}"}),
        ),
        (
            json!({}),
            json!({"turnMetadata":"","turn_metadata":"{\"request_kind\":\"review\"}"}),
        ),
    ] {
        let semantics = codex_responses_request_semantics(&object(body), &object(context));
        assert_eq!(semantics.request_kind, None);
    }
}

fn object(value: Value) -> Map<String, Value> {
    let Value::Object(object) = value else {
        panic!("test value must be an object");
    };
    object
}

#[test]
fn account_identity_uses_logical_metadata_instead_of_parent_cache_routing() {
    use gateway_protocol::openai::codex_account_session_id;
    let context = object(
        json!({"session_id":"parent-cache", "turn_metadata": "{\"session_id\":\"handshake-session\"}"}),
    );
    let body = object(
        json!({"client_metadata":{"x-codex-turn-metadata":"{\"session_id\":\"actual-session\",\"thread_id\":\"child\"}"}}),
    );
    assert_eq!(
        codex_account_session_id(&body, &context).as_deref(),
        Some("actual-session")
    );
    assert_eq!(
        codex_session_id(&body, &context).as_deref(),
        Some("parent-cache")
    );
}

#[test]
fn account_identity_does_not_infer_a_session_from_turn_or_cache_keys() {
    use gateway_protocol::openai::codex_account_session_id;
    for body in [
        json!({"prompt_cache_key":"shared-prompt"}),
        json!({"turn_id":"turn", "thread_id":"child"}),
        json!({"input":"hello"}),
    ] {
        assert_eq!(codex_account_session_id(&object(body), &Map::new()), None);
    }
}

#[test]
fn request_observation_classifies_prewarm_from_generate() {
    for (generate, request_kind, expected_kind) in [
        (Some(json!(false)), None, Some("prewarm")),
        (Some(json!(false)), Some("prewarm"), Some("prewarm")),
        (Some(json!(false)), Some("review"), Some("prewarm")),
        (Some(json!(true)), Some("prewarm"), None),
        (None, Some("prewarm"), None),
        (Some(json!(null)), Some("prewarm"), None),
        (Some(json!("false")), Some("prewarm"), None),
        (None, Some("review"), Some("review")),
        (None, None, None),
    ] {
        let mut body = Map::from_iter([
            ("model".to_owned(), json!("gpt-test")),
            ("input".to_owned(), json!("hello")),
            ("store".to_owned(), json!(false)),
        ]);
        if let Some(generate) = generate {
            body.insert("generate".to_owned(), generate);
        }
        let context = Map::from_iter([(
            "turn_metadata".to_owned(),
            Value::String(
                json!({"request_kind": request_kind, "subagent_kind": "worker"}).to_string(),
            ),
        )]);
        let observation = codex_responses_request_semantics(&body, &context);

        assert_eq!(
            observation.request_kind.as_deref(),
            expected_kind,
            "generate={:?}, request_kind={request_kind:?}",
            body.get("generate"),
        );
        assert_eq!(observation.subagent_kind.as_deref(), Some("worker"));
    }
}
