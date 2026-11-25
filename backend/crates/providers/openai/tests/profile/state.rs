//! 请求画像快照与身份规则测试

use super::*;

#[test]
fn wire_profile_should_generate_bundled_core_app_server_user_agent() {
    let profile = CodexWireProfile {
        client_kind: provider_openai::profile::selection::ClientKind::Desktop,
        originator: "Codex Desktop".to_owned(),
        codex_version: "0.147.0-alpha.6.6".to_owned(),
        desktop_version: "26.803.81509".to_owned(),
        desktop_build: "6415".to_owned(),
        os_type: "Mac OS".to_owned(),
        os_version: "15.7.1".to_owned(),
        arch: "arm64".to_owned(),
        terminal: "unknown".to_owned(),
        exact_user_agent: None,
        installation_seed: None,
        residency: None,
        account_routing_override: None,
        verified_at: Utc
            .with_ymd_and_hms(2026, 8, 3, 0, 0, 0)
            .single()
            .expect("valid fixture time"),
    };

    assert_eq!(
        profile.user_agent(),
        "Codex Desktop/0.147.0-alpha.6.6 (Mac OS 15.7.1; arm64) unknown (Codex Desktop; 26.803.81509)"
    );
}

/// 滞后档位按发布观察序列解析：启动种子、历次核验发布与失败观察共同构成历史。
#[tokio::test]
async fn lagged_profiles_track_observed_release_history() {
    use provider_openai::profile::selection::ClientProfileSelection;

    let state = CodexWireProfileState::new(wire_profile());
    let transport = Arc::new(ReleaseTransport::new(
        [
            Ok(release("26.810.41047", "6570")),
            Ok(release("26.820.41131", "6700")),
            Err(CodexDesktopReleaseError::InvalidDocument),
        ],
        [Ok("0.148.0-alpha.9".to_owned()), Ok("0.149.0".to_owned())],
    ));
    let service = service(
        state.clone(),
        transport,
        Arc::new(ArtifactProfiles::default()),
        None,
    );
    let selection = |lag: Option<u32>| ClientProfileSelection {
        version_lag: lag,
        ..Default::default()
    };

    // 启动种子来自初始画像；首次检查前档位回退到该版本而不是失败。
    assert_eq!(
        selection(Some(8)).resolve(&state).unwrap().codex_version,
        "0.147.0-alpha.6.6"
    );

    service.refresh().await.expect("first release refresh");
    service.refresh().await.expect("second release refresh");

    let latest = selection(None).resolve(&state).unwrap();
    assert_eq!(latest.codex_version, "0.149.0");
    assert_eq!(latest.desktop_version, "26.820.41131");
    // 滞后一档是上一次核验发布，滞后两档落到启动种子。
    let lagged = selection(Some(1)).resolve(&state).unwrap();
    assert_eq!(lagged.codex_version, "0.148.0-alpha.9");
    assert_eq!(lagged.desktop_version, "26.810.41047");
    assert_eq!(
        lagged.user_agent(),
        "Codex Desktop/0.148.0-alpha.9 (Mac OS 15.7.1; arm64) unknown (Codex Desktop; 26.810.41047)"
    );
    assert_eq!(
        selection(Some(2)).resolve(&state).unwrap().codex_version,
        "0.147.0-alpha.6.6"
    );
    // 观察序列不足档位深度时回退最旧已核验版本，不产生新的失败模式。
    assert_eq!(
        selection(Some(8)).resolve(&state).unwrap().codex_version,
        "0.147.0-alpha.6.6"
    );

    // 失败观察只更新状态，发布历史保持不变。
    service
        .refresh()
        .await
        .expect_err("third release refresh should fail");
    assert_eq!(
        selection(Some(1)).resolve(&state).unwrap().codex_version,
        "0.148.0-alpha.9"
    );
}
