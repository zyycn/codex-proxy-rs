//! 官方发行解析、刷新与原子更新测试

use super::*;

#[test]
fn desktop_appcast_should_publish_the_first_complete_release() {
    let release = parse_desktop_release(
        r#"<?xml version="1.0" encoding="utf-8"?>
        <rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
          <channel>
            <item>
              <title>Incomplete release without artifact identity</title>
            </item>
            <item>
              <pubDate>Sun, 19 Jul 2026 08:00:00 +0000</pubDate>
              <sparkle:minimumSystemVersion>14.0</sparkle:minimumSystemVersion>
              <sparkle:hardwareRequirements>arm64</sparkle:hardwareRequirements>
              <enclosure url="https://persistent.oaistatic.com/codex.zip"
                length="123456" sparkle:shortVersionString="26.720.1"
                sparkle:version="72001" sparkle:edSignature="signature" />
            </item>
            <item>
              <enclosure sparkle:shortVersionString="26.719.1" sparkle:version="71901" />
            </item>
          </channel>
        </rss>"#,
    )
    .expect("valid appcast");

    assert_eq!(release.version, "26.720.1");
    assert_eq!(release.build, "72001");
    assert_eq!(release.download_size, Some(123_456));
    assert!(release.signature_present);
}

#[test]
fn desktop_appcast_should_reject_invalid_version_and_build() {
    for (version, build, expected) in [
        ("latest", "72001", CodexDesktopReleaseError::InvalidVersion),
        (
            "26.720.1",
            "build-72001",
            CodexDesktopReleaseError::InvalidBuild,
        ),
    ] {
        let error = parse_desktop_release(&format!(
            r#"<rss xmlns:sparkle="https://sparkle-project.org/xml-namespaces/sparkle"><channel><item><enclosure sparkle:shortVersionString="{version}" sparkle:version="{build}" /></item></channel></rss>"#,
        ))
        .expect_err("invalid release identity");
        assert_eq!(
            std::mem::discriminant(&error),
            std::mem::discriminant(&expected)
        );
    }
}

#[tokio::test]
async fn bundled_release_update_should_change_core_and_desktop_identity_atomically() {
    let original = wire_profile();
    let original_user_agent = original.user_agent();
    let state = CodexWireProfileState::new(original.clone());
    let transport = Arc::new(ReleaseTransport::new(
        [Ok(release("26.810.41047", "6570"))],
        [Ok("0.148.0-alpha.9".to_owned())],
    ));
    let cache = Arc::new(ArtifactProfiles::default());
    let service = service(state.clone(), transport.clone(), cache.clone(), None);

    service.refresh().await.expect("release refresh");

    let updated = state.snapshot();
    assert_eq!(updated.desktop_version, "26.810.41047");
    assert_eq!(updated.desktop_build, "6570");
    assert_eq!(updated.codex_version, "0.148.0-alpha.9");
    assert_eq!(updated.originator, original.originator);
    assert_eq!(updated.os_type, original.os_type);
    assert_eq!(updated.os_version, original.os_version);
    assert_eq!(updated.arch, original.arch);
    assert_eq!(updated.terminal, original.terminal);
    assert!(updated.verified_at > original.verified_at);
    assert_ne!(updated.user_agent(), original_user_agent);
    assert_eq!(
        updated.user_agent(),
        "Codex Desktop/0.148.0-alpha.9 (Mac OS 15.7.1; arm64) unknown (Codex Desktop; 26.810.41047)"
    );
    assert_eq!(transport.core_probes(), 1);
    assert!(cache.profile.lock().expect("artifact profile").is_some());
    let status = service.status().snapshot();
    assert_eq!(status.latest, Some(release("26.810.41047", "6570")));
    assert!(status.checked_at.is_some());
    assert!(status.last_error.is_none());

    // 设置页反复读取预设与预览时，只展示当前生效版本及最近检查时间
    let selection = provider_openai::profile::selection::ClientProfileSelection::default();
    let configuration = selection.document().unwrap();
    let first = state.preview_selection(&configuration).unwrap();
    assert_eq!(
        first.expose_to_provider()["userAgent"],
        selection.resolve(&state).unwrap().user_agent()
    );
    assert!(first.expose_to_provider()["checkedAt"].is_string());
    for _ in 0..3 {
        state.selection_options().unwrap();
        assert_eq!(state.preview_selection(&configuration).unwrap(), first);
    }
    assert_eq!(transport.core_probes(), 1);
}

#[tokio::test(start_paused = true)]
async fn unchanged_verified_release_renews_cache_daily_without_downloading_core_again() {
    let state = CodexWireProfileState::new(wire_profile());
    let latest = release("26.810.41047", "6570");
    let transport = Arc::new(ReleaseTransport::new(
        (0..5).map(|_| Ok(latest.clone())),
        [Ok("0.148.0-alpha.9".to_owned())],
    ));
    let cache = Arc::new(ArtifactProfiles::default());
    let service = service(state.clone(), transport.clone(), cache.clone(), None);

    service.refresh().await.expect("first release refresh");
    let verified = state.snapshot();
    tokio::time::advance(Duration::from_secs(60 * 60)).await;
    service.refresh().await.expect("cached release refresh");
    assert_eq!(cache.writes.load(Ordering::SeqCst), 1);

    tokio::time::advance(Duration::from_secs(23 * 60 * 60)).await;
    cache.fail_writes.store(true, Ordering::SeqCst);
    service.refresh().await.expect_err("renewal failure");
    assert_eq!(state.snapshot(), verified);
    cache.fail_writes.store(false, Ordering::SeqCst);
    service.refresh().await.expect("retry renewal");
    assert_eq!(cache.writes.load(Ordering::SeqCst), 3);
    service.refresh().await.expect("renewed cache");
    assert_eq!(cache.writes.load(Ordering::SeqCst), 3);
    assert_eq!(state.snapshot(), verified);
    assert!(service.status().snapshot().last_error.is_none());

    assert_eq!(transport.core_probes(), 1);
}

#[tokio::test]
async fn failed_release_refresh_should_preserve_the_last_successful_profile_and_release() {
    let state = CodexWireProfileState::new(wire_profile());
    let latest = release("26.810.41047", "6570");
    let transport = Arc::new(ReleaseTransport::new(
        [
            Ok(latest.clone()),
            Err(CodexDesktopReleaseError::InvalidDocument),
        ],
        [Ok("0.148.0-alpha.9".to_owned())],
    ));
    let service = service(
        state.clone(),
        transport,
        Arc::new(ArtifactProfiles::default()),
        None,
    );
    service.refresh().await.expect("first release refresh");
    let successful_profile = state.snapshot();

    service
        .refresh()
        .await
        .expect_err("second release refresh should fail");

    assert_eq!(state.snapshot(), successful_profile);
    let status = service.status().snapshot();
    assert_eq!(status.latest, Some(latest));
    assert_eq!(
        status.last_error.as_deref(),
        Some("Codex Desktop appcast document is invalid")
    );
    assert!(status.checked_at.is_some());
}

#[tokio::test]
async fn cache_failure_should_not_publish_a_partially_verified_profile() {
    let original = wire_profile();
    let state = CodexWireProfileState::new(original.clone());
    let transport = Arc::new(ReleaseTransport::new(
        (0..2).map(|_| Ok(release("26.810.41047", "6570"))),
        (0..2).map(|_| Ok("0.148.0-alpha.9".to_owned())),
    ));
    let cache = Arc::new(ArtifactProfiles::failing());
    let service = service(state.clone(), transport, cache.clone(), None);

    service
        .refresh()
        .await
        .expect_err("cache failure must fail the refresh");

    assert_eq!(state.snapshot(), original);
    assert!(service.status().snapshot().latest.is_none());
    cache.fail_writes.store(false, Ordering::SeqCst);
    service.refresh().await.expect("retry same release");
    assert_eq!(state.snapshot().desktop_build, "6570");
}

/// 重启后首次检查发现新版时，滞后一档应选中从缓存恢复的版本
#[tokio::test]
async fn restored_cache_release_should_stay_in_version_lag_history() {
    use provider_openai::profile::selection::ClientProfileSelection;

    let state = CodexWireProfileState::new(wire_profile());
    let restored = CodexBundledReleaseProfile {
        codex_version: "0.148.0-alpha.9".to_owned(),
        desktop_version: "26.810.41047".to_owned(),
        desktop_build: "6570".to_owned(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 8, 20, 0, 0, 0)
            .single()
            .expect("valid fixture time"),
    };
    let transport = Arc::new(ReleaseTransport::new(
        [Ok(release("26.820.41131", "6700"))],
        [Ok("0.149.0".to_owned())],
    ));
    let service = service(
        state.clone(),
        transport,
        Arc::new(ArtifactProfiles::default()),
        Some(restored),
    );
    let selection = |lag: Option<u32>| ClientProfileSelection {
        version_lag: lag,
        ..Default::default()
    };

    // 恢复版本进入历史队首：最新档位取恢复后的画像，滞后一档解析到启动种子。
    assert_eq!(
        selection(None).resolve(&state).unwrap().codex_version,
        "0.148.0-alpha.9"
    );
    assert_eq!(
        selection(Some(1)).resolve(&state).unwrap().codex_version,
        "0.147.0-alpha.6.6"
    );

    service.refresh().await.expect("first release refresh");

    let latest = selection(None).resolve(&state).unwrap();
    assert_eq!(latest.codex_version, "0.149.0");
    assert_eq!(latest.desktop_version, "26.820.41131");
    // 滞后一档是缓存恢复的版本 B，而不是启动种子 A。
    let lagged = selection(Some(1)).resolve(&state).unwrap();
    assert_eq!(lagged.codex_version, "0.148.0-alpha.9");
    assert_eq!(lagged.desktop_version, "26.810.41047");
    assert_eq!(
        lagged.user_agent(),
        "Codex Desktop/0.148.0-alpha.9 (Mac OS 27.2.1; arm64) unknown (Codex Desktop; 26.810.41047)"
    );
    // 滞后两档落到启动种子，恢复观察与启动种子共同构成启动历史。
    assert_eq!(
        selection(Some(2)).resolve(&state).unwrap().codex_version,
        "0.147.0-alpha.6.6"
    );
}

#[tokio::test]
async fn official_appcast_uses_the_deployment_https_proxy() {
    use provider_openai::profile::OfficialCodexDesktopReleaseTransport;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    if std::env::var_os("CPR_TEST_APPCAST_PROXY").is_some() {
        let transport = OfficialCodexDesktopReleaseTransport::new().unwrap();
        assert!(transport.fetch().await.is_err());
        return;
    }
    let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "profile::release::official_appcast_uses_the_deployment_https_proxy",
        ])
        .env("CPR_TEST_APPCAST_PROXY", "1")
        .env(
            "HTTPS_PROXY",
            format!("http://{}", server.local_addr().unwrap()),
        )
        .env("HTTP_PROXY", "")
        .env("ALL_PROXY", "")
        .env("NO_PROXY", "")
        .env_remove("REQUEST_METHOD");
    let observe = async {
        let (socket, _) = server.accept().await.unwrap();
        let mut socket = BufReader::new(socket);
        let mut line = String::new();
        socket.read_line(&mut line).await.unwrap();
        assert_eq!(line, "CONNECT persistent.oaistatic.com:443 HTTP/1.1\r\n");
        socket
            .get_mut()
            .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    };
    let (output, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(command.output(), observe)
    })
    .await
    .unwrap();
    assert!(output.unwrap().status.success());
}
