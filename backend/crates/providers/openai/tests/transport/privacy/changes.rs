//! 局部隐私事务的跨副本回滚、上下文定位失效与编码大小边界

use super::{policy, rule, sample};
use gateway_core::settings::privacy::*;
use provider_openai::transport::privacy::{compile, preview};
use serde_json::{Value, json};

fn git_item(value: Value) -> Value {
    json!({"role":"user","content":[{"type":"input_text","text":format!(
        "<external_codex_git_state><environment_context><git>{value}</git></environment_context></external_codex_git_state>"
    )}]})
}

#[test]
fn failed_metadata_rule_preserves_body_headers_and_independent_metadata() {
    let mut request = sample(
        vec![rule(
            PrivacyScope::TurnMetadata,
            "$.secret",
            PrivacyAction::RemoveField,
        )],
        json!({"turnMetadata":{"secret":true},"client_metadata":{"turnMetadata":42}}),
    );
    request.policy.on_error = PrivacyFailureMode::SkipRule;
    request.turn_metadata = Some(r#"{"secret":true}"#.into());
    request.headers.insert(
        "x-codex-turn-metadata".into(),
        vec![r#"{"secret":true}"#.into()],
    );
    let output = preview(request.clone()).unwrap();
    assert_eq!(output.body, request.body);
    assert_eq!(output.headers, request.headers);
    assert_eq!(output.turn_metadata, request.turn_metadata);
    assert_eq!(output.outcomes[0].status, "skipped");
}

#[test]
fn malformed_later_git_copy_rolls_back_every_context() {
    let first = git_item(json!({"secret":true}));
    let mut second = first.clone();
    second["content"][0]["text"] = json!(
        "<external_codex_git_state><environment_context><git>invalid</git></environment_context></external_codex_git_state>"
    );
    let body = json!({"input":[first,second]});
    let mut request = sample(
        vec![rule(
            PrivacyScope::DesktopGitContext,
            "$.secret",
            PrivacyAction::RemoveField,
        )],
        body.clone(),
    );
    request.policy.on_error = PrivacyFailureMode::SkipRule;
    let output = preview(request).unwrap();
    assert_eq!(output.body, body);
    assert_eq!(output.outcomes[0].status, "skipped");
}

#[test]
fn body_deletion_invalidates_context_positions_between_rules() {
    let first = rule(
        PrivacyScope::DesktopGitContext,
        "$.first",
        PrivacyAction::RemoveField,
    );
    let mut delete = rule(
        PrivacyScope::RequestBody,
        "$.input[0]",
        PrivacyAction::RemoveField,
    );
    delete.id = "delete".into();
    let mut last = rule(
        PrivacyScope::DesktopGitContext,
        "$.last",
        PrivacyAction::RemoveField,
    );
    last.id = "last".into();
    let output = preview(sample(
        vec![first, delete, last],
        json!({"input":[
            git_item(json!({"first":1})),git_item(json!({"first":1,"last":2,"keep":true}))
        ]}),
    ))
    .unwrap();
    assert_eq!(
        output
            .outcomes
            .iter()
            .map(|o| o.matches)
            .collect::<Vec<_>>(),
        vec![2, 1, 1]
    );
    assert_eq!(
        output.body,
        json!({"input":[git_item(json!({"keep":true}))]})
    );
}

#[test]
fn cached_candidates_recheck_context_wrapper_after_text_replacement() {
    let mut first = rule(
        PrivacyScope::EnvironmentText,
        "$",
        PrivacyAction::RegexReplace,
    );
    first.pattern = Some("<environment_context>".into());
    first.replacement = "<environment_context note=\"test\">".into();
    let mut last = rule(
        PrivacyScope::EnvironmentText,
        "$",
        PrivacyAction::RegexReplace,
    );
    last.id = "last".into();
    last.pattern = Some("private".into());
    last.replacement = "public".into();
    let output = preview(sample(
        vec![first, last],
        json!({"input":[{"role":"user","content":[
            {"type":"input_text","text":"<environment_context>private</environment_context>"}
        ]}]}),
    ))
    .unwrap();
    assert_eq!(output.outcomes[0].status, "applied");
    assert_eq!(output.outcomes[1].status, "unmatched");
    assert!(
        output.body["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("private")
    );
}

#[test]
fn unicode_reencoding_growth_rejects_and_rolls_back_only_current_rule() {
    let mut remove = rule(
        PrivacyScope::TurnMetadata,
        "$.secret",
        PrivacyAction::RemoveField,
    );
    remove.id = "remove".into();
    let mut last = rule(
        PrivacyScope::RequestHeader,
        "x-remove",
        PrivacyAction::RemoveField,
    );
    last.id = "last".into();
    let mut config = policy(vec![remove, last]);
    config.on_error = PrivacyFailureMode::SkipRule;
    let compiled = compile(&config).unwrap();
    let mut body = json!({"turnMetadata":json!({"secret":true,"keep":"界".repeat(100)}).to_string(),"padding":""});
    let padding = 32 * 1024 * 1024 - serde_json::to_vec(&body).unwrap().len();
    body["padding"] = json!("a".repeat(padding));
    let original = body.clone();
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-remove", "test".parse().unwrap());
    let outcomes = compiled
        .apply(&mut body, &mut headers, &mut None, &|| false)
        .unwrap();
    assert_eq!(body, original);
    assert_eq!(
        outcomes[0].reason.as_deref(),
        Some("请求或预览样本超出大小限制")
    );
    assert_eq!(outcomes[1].status, "applied");
    assert!(headers.is_empty());
}

#[test]
fn cancellation_after_staging_leaves_every_carrier_unchanged() {
    let config = policy(vec![rule(
        PrivacyScope::TurnMetadata,
        "$.secret",
        PrivacyAction::RemoveField,
    )]);
    let compiled = compile(&config).unwrap();
    let original =
        json!({"turnMetadata":{"secret":true},"client_metadata":{"turnMetadata":{"secret":true}}});
    // 覆盖每一个取消检查点，包括片段已暂存、尚未提交的最后一步
    let mut successful = false;
    for cancel_at in 1..64 {
        let mut body = original.clone();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "x-codex-turn-metadata",
            r#"{"secret":true}"#.parse().unwrap(),
        );
        let original_headers = headers.clone();
        let mut metadata = Some(r#"{"secret":true}"#.to_owned());
        let original_metadata = metadata.clone();
        let calls = std::cell::Cell::new(0);
        let result = compiled.apply(&mut body, &mut headers, &mut metadata, &|| {
            calls.set(calls.get() + 1);
            calls.get() >= cancel_at
        });
        match result {
            Err(error) => {
                assert_eq!(error.reason, "请求已取消或超时");
                assert_eq!(body, original);
                assert_eq!(headers, original_headers);
                assert_eq!(metadata, original_metadata);
            }
            Ok(outcomes) => {
                assert_eq!(outcomes[0].matches, 4);
                successful = true;
                break;
            }
        }
    }
    assert!(successful);
}
