//! 工作区化名的会话隔离、路径风格、多副本原子性与执行上下文保留

use super::*;

#[test]
fn project_anonymization_preserves_usernames_and_git_information() {
    for path in [
        "/home/alex/projects/private",
        "/Users/alex/private",
        r"C:\Users\alex\private",
    ] {
        let metadata = json!({"associated_remote_urls":{"origin":"https://example.invalid/team/private"},"latest_git_commit_hash":"abc","has_changes":true});
        let mut request = sample(
            vec![rule(
                PrivacyScope::TurnMetadata,
                "$.workspaces",
                PrivacyAction::AnonymizeProject,
            )],
            json!({"turn_metadata":{"workspaces":{path:metadata}}}),
        );
        request
            .headers
            .insert("session-id".into(), vec!["sample-session".into()]);
        let output = preview(request).unwrap();
        let (alias, value) = output.body["turn_metadata"]["workspaces"]
            .as_object()
            .unwrap()
            .iter()
            .next()
            .unwrap();
        assert!(alias.starts_with(path.rsplit_once(['/', '\\']).unwrap().0));
        assert_ne!(alias, path);
        assert_eq!(value, &metadata);
    }
}

#[test]
fn workspace_steps_allow_customizing_username_and_retaining_remotes() {
    for path in [
        "/home/alex/projects/private",
        "/Users/alex/private",
        r"C:\Users\alex\private",
    ] {
        let metadata = json!({"workspaces":{path:{"associated_remote_urls":{"origin":"https://example.invalid/team/private"},"latest_git_commit_hash":"abc","has_changes":true}}});
        let mut customized = workspace_rule("blg");
        customized.steps.remove(2);
        let mut request = sample(
            vec![customized],
            json!({"client_metadata":{"x-codex-turn-metadata":metadata.to_string()}}),
        );
        request.turn_metadata = Some(metadata.to_string());
        request
            .headers
            .insert("session-id".into(), vec!["sample-session".into()]);
        request
            .headers
            .insert("x-codex-turn-metadata".into(), vec![metadata.to_string()]);
        let output = preview(request).unwrap();
        let value: Value = serde_json::from_str(output.turn_metadata.as_deref().unwrap()).unwrap();
        let (alias, workspace) = value["workspaces"]
            .as_object()
            .unwrap()
            .iter()
            .next()
            .unwrap();
        assert!(
            alias.starts_with(
                &path
                    .rsplit_once(['/', '\\'])
                    .unwrap()
                    .0
                    .replace("alex", "blg")
            )
        );
        assert_eq!(
            workspace["associated_remote_urls"],
            metadata["workspaces"][path]["associated_remote_urls"]
        );
        assert!(workspace.get("latest_git_commit_hash").is_none());
    }
}

#[test]
fn aliases_preserve_path_style_and_execution_context_across_all_carriers() {
    for path in [
        "/Users/alex/Desktop/Codes/private",
        "/home/alex/projects/private",
        r"C:\Users\alex\projects\private",
        "/Users/alex/projects/codex-proxy-rs",
        "/home/alex/projects/工具_项目",
        "/home/alex/projects/a",
        "/home/alex/projects/123",
        "/home/alex/projects/---",
    ] {
        let metadata = json!({"workspaces":{path:{"associated_remote_urls":{"origin":"https://example.invalid/team/private"},"latest_git_commit_hash":"abc","has_changes":true}},"request_kind":"turn"});
        let original_input = json!([{"role":"user","content":format!("<cwd>{path}</cwd>")}]);
        let mut request = sample(
            vec![workspace_rule("user")],
            json!({
                "input":original_input,
                "turnMetadata":metadata,
                "turn_metadata":metadata.to_string(),
                "x-codex-turn-metadata":metadata,
                "client_metadata":{"x-codex-turn-metadata":metadata.to_string()}
            }),
        );
        request.turn_metadata = Some(metadata.to_string());
        request.headers.insert(
            "x-codex-turn-metadata".into(),
            vec![metadata.to_string(), metadata.to_string()],
        );
        request
            .headers
            .insert("session-id".into(), vec!["sample-session".into()]);
        let result = preview(request.clone()).unwrap();
        let expected: Value =
            serde_json::from_str(result.turn_metadata.as_deref().unwrap()).unwrap();
        let alias = expected["workspaces"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .unwrap();
        let parent = path
            .rsplit_once(['/', '\\'])
            .unwrap()
            .0
            .replace("alex", "user");
        assert!(alias.starts_with(&parent));
        assert!(!alias.contains("private") && !alias.contains("alex"));
        let name = alias.rsplit(['/', '\\']).next().unwrap();
        let parts = name.split('-').collect::<Vec<_>>();
        assert_eq!(parts.len(), 4);
        for index in [1, 2] {
            assert!((1..=1000).contains(&parts[index].parse::<u16>().unwrap()));
        }
        let mut original = path
            .rsplit(['/', '\\'])
            .next()
            .unwrap()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<Vec<_>>();
        if original.is_empty() {
            original.extend("project".chars());
        }
        let mut shuffled = parts[0].chars().collect::<Vec<_>>();
        original.sort_unstable();
        shuffled.sort_unstable();
        assert_eq!(shuffled, original);
        assert_eq!(expected["workspaces"][alias], json!({"has_changes":true}));
        assert_eq!(result.body["input"], original_input);
        assert_eq!(result.body["turnMetadata"], expected);
        assert_eq!(result.body["x-codex-turn-metadata"], expected);
        for raw in result.headers["x-codex-turn-metadata"]
            .iter()
            .map(String::as_str)
            .chain([
                result.body["turn_metadata"].as_str().unwrap(),
                result.body["client_metadata"]["x-codex-turn-metadata"]
                    .as_str()
                    .unwrap(),
            ])
        {
            assert_eq!(serde_json::from_str::<Value>(raw).unwrap(), expected);
        }
        assert_eq!(result.outcomes[0].matches, 28);
        assert_eq!(preview(request.clone()).unwrap().body, result.body);
        request
            .headers
            .insert("session-id".into(), vec!["another-session".into()]);
        assert_ne!(
            preview(request).unwrap().body["turnMetadata"]["workspaces"],
            expected["workspaces"]
        );
    }
}

#[test]
fn long_workspace_names_produce_valid_directory_components() {
    let path = format!("/workspace/{}", "工程".repeat(40));
    let mut request = sample(
        vec![workspace_rule("user")],
        json!({"turn_metadata":{"workspaces":{path:{}}}}),
    );
    request
        .headers
        .insert("session-id".into(), vec!["long-name".into()]);
    let result = preview(request).unwrap();
    let alias = result.body["turn_metadata"]["workspaces"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap();
    let name = alias.rsplit('/').next().unwrap();
    assert!(name.len() <= 255);
    assert_eq!(name.split('-').next().unwrap().chars().count(), 48);
}

#[test]
fn multiple_workspaces_remain_stable_as_the_workspace_set_grows() {
    let compiled = compile(&policy(vec![workspace_rule("user")])).unwrap();
    let transform = |scope, count| {
        let mut entries = serde_json::Map::new();
        for n in 0..count {
            entries.insert(
                format!("/Users/alex/projects/repo-{n}"),
                json!({"marker":n}),
            );
        }
        let mut body = json!({"turn_metadata":{"workspaces":entries}});
        compiled
            .apply(
                &mut body,
                &mut Default::default(),
                &mut None,
                Some(scope),
                &|| false,
            )
            .unwrap();
        body["turn_metadata"]["workspaces"]
            .as_object()
            .unwrap()
            .clone()
    };
    let first = transform("key-a-session-a", 1);
    let many = transform("key-a-session-a", 100);
    assert_eq!(many.len(), 100);
    for (path, value) in &first {
        assert_eq!(many.get(path), Some(value));
    }
    assert_ne!(first, transform("key-b-session-a", 1));
    assert_ne!(first, transform("key-a-session-b", 1));
}

#[test]
fn workspace_failure_rolls_back_every_copy_and_respects_policy() {
    let original = json!({"turn_metadata":{"workspaces":{"/Users/alex/projects/private":{"latest_git_commit_hash":"abc"}}}});
    for (scope, invalid_header) in [(None, false), (Some("session"), true)] {
        let mut config = policy(vec![workspace_rule("user")]);
        config.on_error = PrivacyFailureMode::SkipRule;
        let mut body = original.clone();
        let mut headers = reqwest::header::HeaderMap::new();
        if invalid_header {
            headers.insert(
                "x-codex-turn-metadata",
                r#"{"workspaces":{"relative/path":{}}}"#.parse().unwrap(),
            );
        }
        let before = headers.clone();
        let result = compile(&config)
            .unwrap()
            .apply(&mut body, &mut headers, &mut None, scope, &|| false)
            .unwrap();
        assert_eq!(result[0].status, "skipped");
        assert_eq!(body, original);
        assert_eq!(headers, before);
        config.on_error = PrivacyFailureMode::RejectRequest;
        assert!(
            compile(&config)
                .unwrap()
                .apply(&mut body, &mut headers, &mut None, scope, &|| false)
                .is_err()
        );
    }
    assert!(
        preview(sample(vec![workspace_rule("user")], json!({})))
            .unwrap()
            .outcomes[0]
            .matches
            == 0
    );
    for (scope, selector) in [
        (PrivacyScope::RequestBody, "$.workspaces"),
        (PrivacyScope::TurnMetadata, "$"),
    ] {
        assert!(
            compile(&policy(vec![rule(
                scope,
                selector,
                PrivacyAction::AnonymizeProject
            )]))
            .is_err()
        );
    }
}
