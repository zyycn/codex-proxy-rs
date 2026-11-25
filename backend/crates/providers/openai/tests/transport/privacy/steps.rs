//! 聚合规则的顺序执行、整条回滚、取消与执行限制

use super::{policy, rule, sample, step};
use gateway_core::settings::privacy::*;
use provider_openai::transport::privacy::{compile, preview};
use serde_json::{Value, json};

#[test]
fn aggregate_steps_read_previous_results_and_report_one_outcome() {
    let mut rename = step(
        PrivacyScope::TurnMetadata,
        "$.workspaces",
        PrivacyAction::RenameKey,
    );
    rename.pattern = Some("^/home/[^/]+/(.*)$".into());
    rename.replacement = "/workspace/${1}".into();
    let mut replace = step(
        PrivacyScope::TurnMetadata,
        "$[\"workspaces\"][\"/workspace/demo\"].owner",
        PrivacyAction::SetValue,
    );
    replace.value = json!("user");
    let mut grouped = rule(
        PrivacyScope::RequestHeader,
        "x-private",
        PrivacyAction::RemoveField,
    );
    grouped.steps = vec![rename, replace, grouped.steps.remove(0)];
    let metadata = json!({"workspaces":{"/home/alice/demo":{"owner":"alice"}}});
    let mut request = sample(vec![grouped], json!({"turnMetadata":metadata}));
    request.turn_metadata = Some(metadata.to_string());
    request
        .headers
        .insert("x-codex-turn-metadata".into(), vec![metadata.to_string()]);
    request
        .headers
        .insert("x-private".into(), vec!["alice".into()]);
    let output = preview(request).unwrap();
    let expected = json!({"workspaces":{"/workspace/demo":{"owner":"user"}}});
    assert_eq!(output.body["turnMetadata"], expected);
    assert_eq!(
        serde_json::from_str::<Value>(output.turn_metadata.as_ref().unwrap()).unwrap(),
        expected
    );
    assert_eq!(
        serde_json::from_str::<Value>(&output.headers["x-codex-turn-metadata"][0]).unwrap(),
        expected
    );
    assert!(!output.headers.contains_key("x-private"));
    assert_eq!(output.outcomes.len(), 1);
    assert_eq!(output.outcomes[0].matches, 7);
}

fn rollback_request() -> PrivacyPreviewRequest {
    let mut grouped = rule(
        PrivacyScope::TurnMetadata,
        "$.first",
        PrivacyAction::RemoveField,
    );
    grouped.steps.extend([
        step(
            PrivacyScope::TurnMetadata,
            "$.second",
            PrivacyAction::RemoveField,
        ),
        step(
            PrivacyScope::RequestBody,
            "$.input[0]",
            PrivacyAction::RemoveField,
        ),
        step(
            PrivacyScope::TurnMetadata,
            "$.third",
            PrivacyAction::RemoveField,
        ),
        step(
            PrivacyScope::RequestHeader,
            "x-private",
            PrivacyAction::RemoveField,
        ),
    ]);
    let metadata = json!({"first":1, "second":2, "third":3, "keep":true});
    let mut request = sample(
        vec![grouped],
        json!({"turnMetadata":metadata,"input":[{"keep":"original"}]}),
    );
    request.turn_metadata = Some(metadata.to_string());
    request
        .headers
        .insert("x-codex-turn-metadata".into(), vec![metadata.to_string()]);
    request
        .headers
        .insert("x-private".into(), vec!["private".into()]);
    request
}

#[test]
fn later_step_failure_rolls_back_overlapping_body_changes_and_all_carriers() {
    for mode in [
        PrivacyFailureMode::SkipRule,
        PrivacyFailureMode::RejectRequest,
    ] {
        let mut request = rollback_request();
        request.policy.on_error = mode;
        let mut fail = step(
            PrivacyScope::RequestHeader,
            "x-codex-turn-metadata",
            PrivacyAction::SetValue,
        );
        fail.value = json!(42);
        request.policy.rules[0].steps.push(fail);
        let compiled = compile(&request.policy).unwrap();
        let mut body = request.body.clone();
        let mut headers = reqwest::header::HeaderMap::new();
        for (key, values) in &request.headers {
            for value in values {
                headers.append(
                    reqwest::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
                    value.parse().unwrap(),
                );
            }
        }
        let original_headers = headers.clone();
        let mut metadata = request.turn_metadata.clone();
        let result = compiled.apply(&mut body, &mut headers, &mut metadata, None, &|| false);
        assert_eq!(body, request.body);
        assert_eq!(headers, original_headers);
        assert_eq!(metadata, request.turn_metadata);
        match mode {
            PrivacyFailureMode::SkipRule => assert_eq!(result.unwrap()[0].status, "skipped"),
            PrivacyFailureMode::RejectRequest => assert_eq!(result.unwrap_err().rule_index, 0),
        }
    }
}

#[test]
fn cancellation_between_steps_rolls_back_the_whole_rule() {
    let request = rollback_request();
    let compiled = compile(&request.policy).unwrap();
    let mut succeeded = false;
    for cancel_at in 1..256 {
        let mut body = request.body.clone();
        let mut metadata = request.turn_metadata.clone();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-private", "private".parse().unwrap());
        let original_headers = headers.clone();
        let calls = std::cell::Cell::new(0);
        let result = compiled.apply(&mut body, &mut headers, &mut metadata, None, &|| {
            calls.set(calls.get() + 1);
            calls.get() >= cancel_at
        });
        if let Ok(outcomes) = result {
            assert_eq!(outcomes[0].status, "applied");
            succeeded = true;
            break;
        }
        assert_eq!(body, request.body);
        assert_eq!(headers, original_headers);
        assert_eq!(metadata, request.turn_metadata);
    }
    assert!(succeeded);
}

#[test]
fn aggregate_respects_disabled_state_and_total_step_limit() {
    let mut grouped = rule(
        PrivacyScope::RequestBody,
        "$.private",
        PrivacyAction::RemoveField,
    );
    grouped.steps.push(step(
        PrivacyScope::RequestHeader,
        "x-private",
        PrivacyAction::RemoveField,
    ));
    grouped.enabled = false;
    let request = sample(vec![grouped.clone()], json!({"private":true}));
    let output = preview(request.clone()).unwrap();
    assert_eq!(output.body, request.body);
    assert_eq!(output.outcomes[0].status, "disabled");
    grouped.steps.clear();
    assert!(compile(&policy(vec![grouped.clone()])).is_err());
    grouped.steps = vec![
        step(
            PrivacyScope::RequestBody,
            "$.private",
            PrivacyAction::RemoveField
        );
        32
    ];
    assert!(compile(&policy(vec![grouped.clone()])).is_ok());
    grouped.steps.push(grouped.steps[0].clone());
    assert!(compile(&policy(vec![grouped])).is_err());
}
