//! 验证 CLI 发行信息的平台依赖一致性与请求画像同步更新

use provider_openai::transport::profile::cli_release::parse_cli_release;
use serde_json::json;

#[test]
fn cli_latest_requires_matching_stable_platform_dependencies() {
    let mut manifest =
        json!({"name":"@openai/codex", "version":"0.155.0", "optionalDependencies": {}});
    for target in [
        "darwin-arm64",
        "darwin-x64",
        "linux-arm64",
        "linux-x64",
        "win32-arm64",
        "win32-x64",
    ] {
        manifest["optionalDependencies"][format!("@openai/codex-{target}")] =
            json!(format!("npm:@openai/codex@0.155.0-{target}"));
    }
    assert_eq!(
        parse_cli_release(&serde_json::to_vec(&manifest).unwrap()).unwrap(),
        "0.155.0"
    );
    let mut missing = manifest.clone();
    missing["optionalDependencies"]
        .as_object_mut()
        .unwrap()
        .remove("@openai/codex-linux-arm64");
    assert!(parse_cli_release(&serde_json::to_vec(&missing).unwrap()).is_err());
    for version in ["0.155.0-alpha.1", "0.155.0+custom", "invalid"] {
        manifest["version"] = json!(version);
        assert!(parse_cli_release(&serde_json::to_vec(&manifest).unwrap()).is_err());
    }
}

#[tokio::test]
async fn official_cli_cache_updates_entry_headers_together_and_preserves_frozen_identities() {
    use std::sync::Arc;
    use std::time::Duration;

    use gateway_core::account::OpaqueProviderData;
    use gateway_core::provider_ports::{ProviderArtifactProfile, ProviderArtifactProfileCachePort};
    use gateway_core::routing::ProviderKind;
    use provider_openai::transport::headers::build_codex_model_headers;
    use provider_openai::transport::profile::CodexWireProfileState;
    use provider_openai::transport::profile::cli_release::CliReleaseService;
    use provider_openai::transport::profile::identity::RequestProfileSelection;
    use provider_openai::transport::profile::selection::{
        CliEntry, ClientKind, ClientPlatform, ClientProfileSelection, VersionMode,
    };

    let state = CodexWireProfileState::new(super::wire_profile());
    let cache = Arc::new(super::ArtifactProfiles::default());
    let provider = ProviderKind::new("openai").unwrap();
    let service = CliReleaseService::new(provider.clone(), state.clone(), cache.clone()).unwrap();
    let automatic = ClientProfileSelection {
        client: ClientKind::Cli,
        platform: ClientPlatform::Linux,
        cli_entry: Some(CliEntry::Tui),
        os_type: Some("Alpine Linux".into()),
        os_version: Some("3.24.1".into()),
        terminal: Some("xterm-256color".into()),
        ..Default::default()
    };
    let frozen = automatic.resolve(&state).unwrap();
    // 从当前基线构造后继发布，升级离线版本无需重写缓存更新场景
    let mut next = semver::Version::parse(&frozen.codex_version).unwrap();
    next.patch += 1;
    let next_sequence = 1 + next.major * 1_000_000 + next.minor * 1_000 + next.patch;
    let next_version = next.to_string();
    let fixed = ClientProfileSelection {
        version_mode: VersionMode::Fixed,
        codex_version: Some(frozen.codex_version.clone()),
        ..automatic.clone()
    };
    let custom = RequestProfileSelection::parse(&OpaqueProviderData::new(
        json!({"mode":"custom", "userAgent":frozen.user_agent()})
            .as_object()
            .unwrap()
            .clone(),
    ))
    .unwrap();

    // 复用官方版本服务的恢复入口，模拟每日检查已核验并写入的下一份发布资料
    cache
        .replace_if_newer(
            ProviderArtifactProfile::new(
                provider.clone(),
                "cli-linux-x64".into(),
                next_sequence,
                chrono::Utc::now().into(),
                OpaqueProviderData::new(
                    json!({"version":next_version}).as_object().unwrap().clone(),
                ),
            ),
            Duration::from_secs(86400),
        )
        .await
        .unwrap();
    service.restore().await;
    for (entry, originator) in [(CliEntry::Tui, "codex-tui"), (CliEntry::Exec, "codex_exec")] {
        let selection = ClientProfileSelection {
            cli_entry: Some(entry),
            ..automatic.clone()
        };
        let current = selection.resolve(&state).unwrap();
        let headers = build_codex_model_headers(&current, "Bearer fixture", None).unwrap();
        let expected = format!(
            "{originator}/{next_version} (Alpine Linux 3.24.1; x86_64) xterm-256color ({originator}; {next_version})"
        );
        assert_eq!(headers["user-agent"], expected);
        assert_eq!(headers["originator"], originator);
        assert_eq!(headers["version"], next_version.as_str());
    }
    assert_eq!(
        fixed.resolve(&state).unwrap().user_agent(),
        frozen.user_agent()
    );
    assert_eq!(
        custom.resolve(&state).unwrap().user_agent(),
        frozen.user_agent()
    );

    // 缓存恢复与启动种子构成观察历史，滞后档位据此钉住上一版。
    let lagged = ClientProfileSelection {
        version_lag: Some(1),
        ..automatic.clone()
    };
    assert_eq!(
        lagged.resolve(&state).unwrap().user_agent(),
        frozen.user_agent()
    );

    let restarted = CodexWireProfileState::new(super::wire_profile());
    CliReleaseService::new(provider, restarted.clone(), cache)
        .unwrap()
        .restore()
        .await;
    assert_eq!(
        automatic.resolve(&restarted).unwrap(),
        automatic.resolve(&state).unwrap()
    );
}
