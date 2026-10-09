//! 隐私规则的多承载一致性、自由字段配置、条件删除与有界失败合同

use gateway_core::settings::privacy::*;
use provider_openai::transport::privacy::{compile, preview};
use serde_json::{Value, json};

pub(crate) fn rule(scope: PrivacyScope, selector: &str, action: PrivacyAction) -> PrivacyRule {
    PrivacyRule {
        id: "rule-1".into(),
        name: "测试规则".into(),
        enabled: true,
        scope,
        selector: selector.into(),
        action,
        pattern: None,
        replacement: String::new(),
        value: Value::Null,
        replace_all: true,
        case_insensitive: false,
        multi_line: false,
    }
}

pub(crate) fn policy(rules: Vec<PrivacyRule>) -> CodexPrivacyPolicy {
    CodexPrivacyPolicy {
        enabled: true,
        on_error: PrivacyFailureMode::RejectRequest,
        rules,
    }
}

fn sample(rules: Vec<PrivacyRule>, body: Value) -> PrivacyPreviewRequest {
    PrivacyPreviewRequest {
        policy: policy(rules),
        body,
        headers: Default::default(),
        turn_metadata: None,
    }
}

#[test]
fn privacy_removes_workspaces_from_every_copy_without_merging_metadata() {
    let workspaces = json!({"/home/alex/项目": {"associated_remote_urls": {"origin":"https://example.test/private/repo"}}});
    let header = json!({"workspaces":workspaces,"request_kind":"primary"});
    let client = json!({"workspaces":workspaces,"tool_namespaces_info":{"tools":["a"]}});
    let mut request = sample(
        vec![rule(
            PrivacyScope::TurnMetadata,
            "$.workspaces",
            PrivacyAction::RemoveField,
        )],
        json!({
            "turnMetadata": header.to_string(), "turn_metadata": header, "x-codex-turn-metadata": header,
            "client_metadata": {"x-codex-turn-metadata":client.to_string()}
        }),
    );
    request.turn_metadata = Some(header.to_string());
    request.headers.insert(
        "x-codex-turn-metadata".into(),
        vec![header.to_string(), header.to_string()],
    );
    let output = preview(request).unwrap();
    assert_eq!(output.outcomes[0].matches, 7);
    assert_eq!(
        output.turn_metadata.as_deref(),
        Some("{\"request_kind\":\"primary\"}")
    );
    assert_eq!(
        output.body["turn_metadata"],
        json!({"request_kind":"primary"})
    );
    assert_eq!(
        serde_json::from_str::<Value>(
            output.body["client_metadata"]["x-codex-turn-metadata"]
                .as_str()
                .unwrap()
        )
        .unwrap(),
        json!({"tool_namespaces_info":{"tools":["a"]}})
    );
    assert!(
        output.headers["x-codex-turn-metadata"]
            .iter()
            .all(|raw| !raw.contains("workspaces"))
    );
}

#[test]
fn privacy_conditionally_deletes_values_and_renames_path_keys_in_sequence() {
    let mut remove = rule(
        PrivacyScope::RequestBody,
        "$.workspaces.*.associated_remote_urls.*",
        PrivacyAction::RemoveField,
    );
    remove.pattern = Some("/private-org/".into());
    let mut rename = rule(
        PrivacyScope::RequestBody,
        "$.workspaces",
        PrivacyAction::RenameKey,
    );
    rename.id = "rename".into();
    rename.pattern = Some("^/home/[^/]+/(.*)$".into());
    rename.replacement = "/workspace/${1}".into();
    let output = preview(sample(vec![remove, rename], json!({"workspaces":{
        "/home/alex/demo":{"associated_remote_urls":{"origin":"https://host/private-org/demo","upstream":"https://host/public/demo"}}
    }}))).unwrap();
    assert_eq!(
        output.body,
        json!({"workspaces":{"/workspace/demo":{"associated_remote_urls":{"upstream":"https://host/public/demo"}}}})
    );
    let mut remove = rule(
        PrivacyScope::RequestBody,
        "$.remotes[*]",
        PrivacyAction::RemoveField,
    );
    remove.pattern = Some("private".into());
    assert_eq!(
        preview(sample(
            vec![remove],
            json!({"remotes":["private-a","public","private-b","private-c"]})
        ))
        .unwrap()
        .body,
        json!({"remotes":["public"]})
    );
}

#[test]
fn privacy_has_no_field_protection_list_in_any_scope() {
    for name in [
        "authorization",
        "cookie",
        "chatgpt-account-id",
        "x-codex-turn-state",
        "x-codex-installation-id",
    ] {
        let mut request = sample(
            vec![rule(
                PrivacyScope::RequestHeader,
                name,
                PrivacyAction::RemoveField,
            )],
            json!({}),
        );
        request
            .headers
            .insert(name.into(), vec!["synthetic-value".into()]);
        assert!(preview(request).unwrap().headers.is_empty(), "{name}");
    }
    for name in [
        "session_id",
        "thread_id",
        "previous_response_id",
        "prompt_cache_key",
        "tool_namespaces_info",
        "history_ingest_requested",
        "analytics_enabled",
    ] {
        let output = preview(sample(
            vec![rule(
                PrivacyScope::RequestBody,
                &format!("$.{name}"),
                PrivacyAction::RemoveField,
            )],
            json!({name:"synthetic"}),
        ))
        .unwrap();
        assert_eq!(output.body, json!({}), "{name}");
    }
}

#[test]
fn privacy_rolls_back_all_copies_of_a_failed_rule_and_continues_in_order() {
    let mut first = rule(
        PrivacyScope::TurnMetadata,
        "$.workspace",
        PrivacyAction::RegexReplace,
    );
    first.pattern = Some("private".into());
    first.replacement = "public".into();
    let mut second = rule(PrivacyScope::RequestBody, "$.keep", PrivacyAction::SetValue);
    second.id = "second".into();
    second.value = json!("done");
    let body = json!({"turnMetadata":{"workspace":"private"},"client_metadata":{"turnMetadata":{"workspace":42}},"keep":"start"});
    let mut request = sample(vec![first.clone(), second], body.clone());
    request.policy.on_error = PrivacyFailureMode::SkipRule;
    let output = preview(request).unwrap();
    assert_eq!(output.body["turnMetadata"], body["turnMetadata"]);
    assert_eq!(output.body["keep"], "done");
    assert_eq!(output.outcomes[0].status, "skipped");
    assert_eq!(output.outcomes[1].status, "applied");
    assert_eq!(
        preview(sample(vec![first], body)).err().unwrap().rule_index,
        0
    );
}

#[test]
fn privacy_key_collisions_fail_without_losing_values() {
    let mut rename = rule(
        PrivacyScope::RequestBody,
        "$.workspaces",
        PrivacyAction::RenameKey,
    );
    rename.pattern = Some(".+".into());
    rename.replacement = "same".into();
    let body = json!({"workspaces":{"first":1,"second":2}});
    let mut request = sample(vec![rename], body.clone());
    request.policy.on_error = PrivacyFailureMode::SkipRule;
    let output = preview(request).unwrap();
    assert_eq!(output.body, body);
    assert_eq!(output.outcomes[0].status, "skipped");
}

#[test]
fn privacy_desktop_context_is_classified_and_reencoded_without_touching_prose() {
    let text = "<external_codex_git_state><environment_context><git source=\"app\" trust=\"untrusted\">{\"workspaces\":{\"/private\":{}},\"note\":\"a &amp; b\"}</git></environment_context></external_codex_git_state>";
    let mut replacement = rule(
        PrivacyScope::DesktopGitContext,
        "$.workspaces",
        PrivacyAction::RemoveField,
    );
    let body = json!({"input":[{"role":"user","content":[{"type":"input_text","text":text},{"type":"input_text","text":text}],"internal_chat_message_metadata_passthrough":{"content_item_kinds":["additional_content.codex_git_state","user.text"]}}]});
    let output = preview(sample(vec![replacement.clone()], body)).unwrap();
    assert!(
        !output.body["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("workspaces")
    );
    assert!(
        output.body["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("a &amp; b")
    );
    assert_eq!(output.body["input"][0]["content"][1]["text"], text);
    replacement.scope = PrivacyScope::EnvironmentText;
    replacement.selector = "$".into();
    replacement.action = PrivacyAction::RegexReplace;
    replacement.pattern = Some("Asia/Shanghai".into());
    replacement.replacement = "UTC".into();
    let body = json!({"input":[{"role":"developer","content":[{"type":"input_text","text":"<codex_apps_client_time_context><timezone>Asia/Shanghai</timezone></codex_apps_client_time_context>"}]}]});
    assert!(
        preview(sample(vec![replacement], body))
            .unwrap()
            .body
            .to_string()
            .contains("<timezone>UTC</timezone>")
    );
}

#[test]
fn privacy_validates_regex_paths_captures_and_execution_limits() {
    let mut replace = rule(
        PrivacyScope::RequestBody,
        "$[\"a.b\"][0]",
        PrivacyAction::RegexReplace,
    );
    replace.pattern = Some("(?P<word>a)".into());
    replace.replacement = "${word}${1}$$".into();
    assert_eq!(
        preview(sample(vec![replace.clone()], json!({"a.b":["a"]})))
            .unwrap()
            .body,
        json!({"a.b":["aa$"]})
    );
    replace.replacement = "${missing}".into();
    assert!(compile(&policy(vec![replace.clone()])).is_err());
    replace.replacement.clear();
    replace.pattern = Some("(?<=a)b".into());
    assert!(compile(&policy(vec![replace])).is_err());
    for path in ["$..secret", "$[?(@.secret)]", "$['single']"] {
        assert!(
            compile(&policy(vec![rule(
                PrivacyScope::RequestBody,
                path,
                PrivacyAction::RemoveField
            )]))
            .is_err()
        );
    }
    let policy = compile(&policy(vec![rule(
        PrivacyScope::RequestBody,
        "$.items.*.missing",
        PrivacyAction::RemoveField,
    )]))
    .unwrap();
    let mut body = json!({"items":vec![json!({}); 10_001]});
    let original = body.clone();
    let error = policy
        .apply(&mut body, &mut Default::default(), &mut None, &|| false)
        .unwrap_err();
    assert_eq!(error.reason, "规则执行预算超限");
    assert_eq!(body, original);
    assert!(
        policy
            .apply(&mut json!({}), &mut Default::default(), &mut None, &|| true)
            .is_err()
    );
}

#[test]
fn privacy_disabled_policy_and_absent_fields_preserve_original_bytes() {
    let body = json!({"turnMetadata":"{  \"keep\" : 1 }"});
    let request = sample(
        vec![rule(
            PrivacyScope::TurnMetadata,
            "$.workspaces",
            PrivacyAction::RemoveField,
        )],
        body.clone(),
    );
    assert_eq!(preview(request.clone()).unwrap().body, body);
    let mut request = request;
    request.policy.enabled = false;
    assert_eq!(preview(request).unwrap().outcomes[0].status, "disabled");
}

#[test]
fn privacy_context_parser_handles_quoted_brackets_and_bounds_xml_depth() {
    let text = "<external_codex_git_state><environment_context><git source=\"a>b\"><![CDATA[{\"workspaces\":{},\"note\":\"a & b\"}]]></git></environment_context></external_codex_git_state>";
    let body = json!({"input":[{"role":"user","content":[{"type":"input_text","text":text}]}]});
    let output = preview(sample(
        vec![rule(
            PrivacyScope::DesktopGitContext,
            "$.workspaces",
            PrivacyAction::RemoveField,
        )],
        body,
    ))
    .unwrap();
    let changed = output.body["input"][0]["content"][0]["text"]
        .as_str()
        .unwrap();
    let document = roxmltree::Document::parse(changed).unwrap();
    let git = document
        .descendants()
        .find(|node| node.has_tag_name("git"))
        .unwrap();
    assert_eq!(git.attribute("source"), Some("a>b"));
    assert_eq!(
        serde_json::from_str::<Value>(git.text().unwrap()).unwrap(),
        json!({"note":"a & b"})
    );
    let nested = format!(
        "<environment_context>{}value{}</environment_context>",
        "<n>".repeat(100),
        "</n>".repeat(100)
    );
    let mut replace = rule(
        PrivacyScope::EnvironmentText,
        "$",
        PrivacyAction::RegexReplace,
    );
    replace.pattern = Some("value".into());
    replace.replacement = "changed".into();
    let body = json!({"input":[{"role":"user","content":[{"type":"input_text","text":nested}]}]});
    assert_eq!(
        preview(sample(vec![replace], body)).err().unwrap().reason,
        "上下文 XML 嵌套超过 64 层"
    );
}

#[test]
fn privacy_reports_headers_that_websocket_opening_cannot_serialize() {
    let mut replace = rule(
        PrivacyScope::RequestHeader,
        "x-label",
        PrivacyAction::SetValue,
    );
    replace.value = json!("别名");
    let mut request = sample(vec![replace], json!({}));
    request
        .headers
        .insert("x-label".into(), vec!["original".into()]);
    assert_eq!(
        preview(request.clone()).err().unwrap().reason,
        "替换结果必须是 ASCII 请求头值"
    );
    request.policy.on_error = PrivacyFailureMode::SkipRule;
    let result = preview(request).unwrap();
    assert_eq!(result.headers["x-label"], vec!["original"]);
    assert_eq!(result.outcomes[0].status, "skipped");
}
