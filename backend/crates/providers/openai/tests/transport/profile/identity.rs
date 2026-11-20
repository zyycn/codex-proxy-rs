//! 验证自定义客户端身份、配套头部与注入防护

use gateway_core::account::OpaqueProviderData;
use provider_openai::transport::headers::build_codex_model_headers;
use provider_openai::transport::profile::identity::RequestProfileSelection;
use provider_openai::transport::profile::selection::ClientProfileSelection;
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use serde_json::{Value, json};

fn document(value: Value) -> OpaqueProviderData {
    OpaqueProviderData::new(value.as_object().unwrap().clone())
}

#[test]
fn batch_identity_changes_keep_each_keys_seed_and_independent_selection() {
    use provider_openai::transport::profile::identity::merge_batch_key_profile;

    let preset = json!({"client": "cli", "platform":"linux", "versionMode":"latest"});
    let custom = json!({"mode":"custom","userAgent":"codex-tui/0.156.1"});
    let replacement = "d3228d4e-7c6f-4d77-a245-89baf5f3959c";
    for seed in [
        "5d774376-8481-43d1-8b0c-b739ef937b05",
        "6da3531b-628d-4e9b-b8a8-5c582c7721bd",
    ] {
        let inherited = json!({"inherit":{"installationSeed":seed}});
        let mut previous = custom.clone();
        previous["installationSeed"] = json!(seed);
        let mut expected = preset.clone();
        expected["installationSeed"] = json!(seed);
        for current in [&previous, &inherited] {
            let current = document(current.clone());
            assert_eq!(
                merge_batch_key_profile(
                    Some(&current),
                    Some(&Some(document(preset.clone()))),
                    None
                )
                .unwrap(),
                Some(document(expected.clone()))
            );
            assert_eq!(
                merge_batch_key_profile(Some(&current), Some(&None), None).unwrap(),
                Some(document(inherited.clone()))
            );
        }
        let next = merge_batch_key_profile(
            Some(&document(previous)),
            None,
            Some(replacement.parse().unwrap()),
        )
        .unwrap()
        .unwrap();
        let mut expected = custom.clone();
        expected["installationSeed"] = json!(replacement);
        assert_eq!(next, document(expected));
    }
    assert_eq!(
        merge_batch_key_profile(None, None, Some(replacement.parse().unwrap())).unwrap(),
        Some(document(
            json!({"inherit":{"installationSeed":replacement}})
        ))
    );
    assert_eq!(
        merge_batch_key_profile(Some(&document(preset)), Some(&None), None).unwrap(),
        None
    );
    for embedded in [
        json!({"installationSeed":replacement}),
        json!({"inherit":{"installationSeed":replacement}}),
    ] {
        assert!(
            merge_batch_key_profile(
                None,
                Some(&Some(document(embedded))),
                Some(replacement.parse().unwrap())
            )
            .is_err()
        );
    }
}

#[test]
fn complete_custom_identities_send_exact_user_agent_and_matching_companion_headers() {
    let state = CodexWireProfileState::new(CodexWireProfile {
        residency: Some("us".to_owned()),
        account_routing_override: Some("us_cr".to_owned()),
        ..Default::default()
    });
    for (core, originator, user_agent) in [
        (
            "0.155.0-alpha.16.3",
            "Codex Desktop",
            "Codex Desktop/0.155.0-alpha.16.3 (Ubuntu 24.4.0; x86_64) unknown (Codex Desktop; 26.917.62051)",
        ),
        (
            "0.156.1",
            "codex-tui",
            "codex-tui/0.156.1 (Ubuntu 24.4.0; x86_64) xterm-256color (codex-tui; 0.156.1)",
        ),
        (
            "0.156.1",
            "codex_exec",
            "codex_exec/0.156.1 (Ubuntu 24.4.0; x86_64) xterm-256color (codex_exec; 0.156.1)",
        ),
    ] {
        let selection = document(json!({
            "mode":"custom", "userAgent":user_agent
        }));
        let profile = RequestProfileSelection::parse(&selection)
            .unwrap()
            .resolve(&state)
            .unwrap();
        let headers = build_codex_model_headers(&profile, "Bearer fixture", None).unwrap();
        assert_eq!(headers["user-agent"], user_agent);
        assert_eq!(headers["originator"], originator);
        assert_eq!(headers["version"], core);
        assert_eq!(headers["x-openai-internal-codex-residency"], "us");
        assert_eq!(headers["x-openai-account-routing-override"], "us_cr");
        assert!(profile.desktop_build.is_empty());
        let frozen: CodexWireProfile =
            serde_json::from_value(serde_json::to_value(&profile).unwrap()).unwrap();
        assert_eq!(frozen.user_agent(), user_agent);
    }
    assert!(state.snapshot().exact_user_agent.is_none());
}

#[test]
fn custom_known_prefix_derives_headers_and_unknown_identity_requires_them() {
    let state = CodexWireProfileState::new(CodexWireProfile::default());
    for (user_agent, originator, core, recognized) in [
        (
            "codex_cli_rs/0.156.1 (Linux 6.8.0; x86_64) custom-terminal",
            "codex_cli_rs",
            "0.156.1",
            true,
        ),
        (
            "My Agent/7 (custom environment)",
            "my-agent",
            "0.156.0",
            false,
        ),
    ] {
        let mut configuration = json!({"mode":"custom","userAgent":user_agent});
        if !recognized {
            assert!(RequestProfileSelection::parse(&document(configuration.clone())).is_err());
            configuration["originator"] = json!(originator);
            configuration["codexVersion"] = json!(core);
        }
        let configuration = document(configuration);
        let profile = RequestProfileSelection::parse(&configuration)
            .unwrap()
            .resolve(&state)
            .unwrap();
        let headers = build_codex_model_headers(&profile, "Bearer fixture", None).unwrap();
        assert_eq!(headers["user-agent"], user_agent);
        assert_eq!(headers["originator"], originator);
        assert_eq!(headers["version"], core);
        let preview = state.preview_selection(&configuration).unwrap();
        assert_eq!(preview.expose_to_provider()["recognized"], recognized);
        assert_eq!(preview.expose_to_provider()["versionSource"], "custom");
        assert!(preview.expose_to_provider()["verifiedAt"].is_null());
    }
}

#[test]
fn raw_identity_rejects_injection_conflicting_headers_and_unknown_modes() {
    for configuration in [
        json!({"mode":"custom","userAgent":"codex-tui/0.156.1\r\nx-injected: yes"}),
        json!({"mode":"custom","userAgent":"codex-tui/0.156.1\tterminal"}),
        json!({"mode":"custom","userAgent":"自定义UA","originator":"custom","codexVersion":"0.156.1"}),
        json!({"mode":"custom","userAgent":"x".repeat(4097),"originator":"custom","codexVersion":"0.156.1"}),
        json!({"mode":"custom","userAgent":"codex-tui/0.156.1","originator":"codex_exec"}),
        json!({"mode":"custom","userAgent":"codex-tui/0.156.1","codexVersion":"0.155.0"}),
        json!({"mode":"custom","userAgent":"codex-tui/latest","originator":"codex-tui","codexVersion":"0.156.1"}),
        json!({"mode":"custom","userAgent":"custom","originator":"agent\r\nx-injected: yes","codexVersion":"0.156.1"}),
        json!({"mode":"custom","userAgent":"custom","originator":"agent","codexVersion":"latest"}),
        json!({"mode":"custom","userAgent":"codex-tui/0.156.1","extraHeader":"anything"}),
        json!({"mode":"unknown","client":"cli","platform":"linux","versionMode":"latest"}),
        json!({"mode":null,"client":"cli","platform":"linux","versionMode":"latest"}),
    ] {
        assert!(
            RequestProfileSelection::parse(&document(configuration.clone())).is_err(),
            "{configuration}"
        );
    }
}

#[test]
fn preset_identity_round_trip_preserves_request_headers_and_official_preview() {
    let state = CodexWireProfileState::new(CodexWireProfile::default());
    let configuration = ClientProfileSelection::default().document().unwrap();
    let resolved = RequestProfileSelection::parse(&configuration)
        .unwrap()
        .resolve(&state)
        .unwrap();
    let restored: CodexWireProfile =
        serde_json::from_value(serde_json::to_value(&resolved).unwrap()).unwrap();

    assert_eq!(
        build_codex_model_headers(&restored, "Bearer fixture", None).unwrap(),
        build_codex_model_headers(&resolved, "Bearer fixture", None).unwrap()
    );
    assert_eq!(
        state
            .preview_selection(&configuration)
            .unwrap()
            .expose_to_provider()["versionSource"],
        "official"
    );
}
