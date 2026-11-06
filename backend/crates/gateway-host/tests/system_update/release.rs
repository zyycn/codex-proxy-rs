//! 发行查询、缓存、版本通道与目标策略测试

use super::*;

#[tokio::test]
async fn update_detail_should_record_decode_failure_while_returning_safe_warning() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not release JSON"))
        .mount(&server)
        .await;
    let fixture = Fixture::new();
    let service = fixture.service(&server);
    let diagnostics = Arc::new(RecordingDiagnostics::default());
    service.set_diagnostics(diagnostics.clone());

    let detail = service
        .update_detail(true, None)
        .await
        .expect("failure detail");
    assert_eq!(detail.warning.as_deref(), Some("GitHub 发行信息格式无效"));
    let failures = diagnostics.failures.lock().expect("diagnostic failures");
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].operation, "check_release");
    let details = failures[0].details.as_ref().expect("decoding details");
    assert!(details.as_str().contains("expected ident"));
    assert!(!details.as_str().contains(&server.uri()));
}

#[tokio::test]
async fn update_detail_should_report_refresh_failure_and_clear_previous_update() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture.mount_release_once(&server, TARGET_VERSION).await;
    let service = fixture.service(&server);
    service
        .update_detail(true, None)
        .await
        .expect("prime cache");

    let detail = service
        .update_detail(true, None)
        .await
        .expect("failure detail");
    assert!(!detail.has_update);
    assert_eq!(
        detail.warning.as_deref(),
        Some("GitHub 发行查询失败（HTTP 404）")
    );
    assert!(!detail.cached);
    assert_eq!(detail.latest_version, "1.0.0");
    assert!(detail.notes.is_none());
    let version = service
        .version()
        .await
        .expect("version after failed refresh");
    assert!(!version.has_update);
    assert!(!version.update_cached);
    assert!(version.update_warning.is_none());
}

#[tokio::test]
async fn update_detail_should_offer_latest_non_draft_release_in_current_channel() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "tag_name": "v1.1.0-beta.3",
                "name": "Draft 1.1.0-beta.3",
                "prerelease": true,
                "draft": true,
                "assets": [],
            },
            {
                "tag_name": "v1.1.0-beta.2",
                "name": "Release 1.1.0-beta.2",
                "body": "notes",
                "html_url": "https://github.com/owner/repository/releases",
                "prerelease": true,
                "assets": [],
            },
        ])))
        .mount(&server)
        .await;
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    config.version = "1.1.0-beta.1".to_owned();
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);

    let detail = service.update_detail(true, None).await.expect("detail");
    assert_eq!(detail.latest_version, "1.1.0-beta.2");
    assert!(detail.has_update);
}

#[tokio::test]
async fn update_detail_should_reject_untrusted_github_api_base() {
    let fixture = Fixture::new();
    let service = ProcessSystemOperations::new(
        CancellationToken::new(),
        fixture.config("https://api.github.example/repos"),
    );

    let detail = service
        .update_detail(true, None)
        .await
        .expect("safe rejection");
    assert!(!detail.update_supported);
    assert!(detail.warning.is_some() || detail.unsupported_reason.is_some());
}

#[tokio::test]
async fn update_detail_should_withhold_updates_for_source_builds() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    config.build_type = "source".to_owned();
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);

    let detail = service.update_detail(true, None).await.expect("detail");
    assert_eq!(detail.latest_version, "1.0.0");
    assert!(!detail.update_supported);
    assert!(detail.unsupported_reason.is_some());
    assert!(!detail.has_update);
    assert!(detail.notes.is_none());
    assert!(detail.release_url.is_none());
    assert!(!service.version().await.expect("version").has_update);
    assert!(
        server
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
}

#[tokio::test]
async fn experimental_build_should_ignore_newer_stable_and_other_experimental_releases() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "tag_name": "v4.0.0", "prerelease": false },
            { "tag_name": "v3.12.0", "prerelease": false },
            { "tag_name": "v3.10.0", "prerelease": false },
            { "tag_name": "v3.11.0-exp.3", "prerelease": true },
            { "tag_name": "v3.10.1-exp.3", "prerelease": true },
            { "tag_name": "v3.11.0-beta.1", "prerelease": true },
            { "tag_name": "v3.10.0-exp.other.3", "prerelease": true },
            { "tag_name": "v3.10.0-exp.3", "prerelease": true, "draft": true },
            { "tag_name": "v3.10.0-exp.4", "prerelease": false },
            { "tag_name": "invalid", "prerelease": true }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    config.version = "3.10.0-exp.2".to_owned();
    config.build_type = "experimental".to_owned();
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);

    let detail = service.update_detail(true, None).await.expect("detail");
    assert_eq!(detail.latest_version, "3.10.0-exp.2");
    assert!(!detail.has_update);
    assert!(detail.update_supported);
    assert!(detail.notes.is_none());
    assert!(detail.release_url.is_none());
    assert!(detail.warning.is_none());
    let version = service.version().await.expect("version");
    assert!(!version.has_update);
    assert_eq!(version.update_channel, "exp");
    let cached = service
        .update_detail(false, None)
        .await
        .expect("cached detail");
    assert!(cached.cached);
    assert!(!cached.has_update);
}

#[tokio::test]
async fn experimental_update_should_find_highest_same_channel_version_across_pages() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let mut first_page = vec![
        serde_json::json!({
            "tag_name": "v3.12.0", "prerelease": false
        });
        100
    ];
    first_page[1] = serde_json::json!({ "tag_name": "v3.10.0-exp.3", "prerelease": true });
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .and(query_param("page", "1"))
        .and(query_param("per_page", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(first_page))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "tag_name": "v3.10.0-exp.10", "prerelease": true, "body": "exp.10 notes" },
            { "tag_name": "v3.10.0-exp.2", "prerelease": true }
        ])))
        .mount(&server)
        .await;
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    config.version = "3.10.0-exp.2".to_owned();
    config.build_type = "experimental".to_owned();
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);

    let detail = service.update_detail(true, None).await.expect("detail");
    assert_eq!(detail.latest_version, "3.10.0-exp.10");
    assert_eq!(detail.notes.as_deref(), Some("exp.10 notes"));
    assert!(detail.has_update);
    assert!(detail.update_supported);
    assert!(service.version().await.expect("version").has_update);
}

#[tokio::test]
async fn update_checks_should_follow_stage_promotion_and_experimental_isolation() {
    let cases = [
        ("3.12.0-alpha.1", "alpha", [true, true, true, true, false]),
        ("3.12.0-beta.1", "beta", [false, true, true, true, false]),
        ("3.12.0-rc.1", "rc", [false, false, true, true, false]),
        ("3.12.0-exp.1", "exp", [false, false, false, false, true]),
        ("3.11.0", "stable", [false, false, false, true, false]),
    ];
    for (current, channel, expected) in cases {
        for (target, allowed) in [
            "3.12.0-alpha.2",
            "3.12.0-beta.2",
            "3.12.0-rc.2",
            "3.12.0",
            "3.12.0-exp.2",
        ]
        .into_iter()
        .zip(expected)
        {
            let server = MockServer::start().await;
            let fixture = Fixture::new();
            fixture.mount_release_once(&server, target).await;
            let mut config = fixture.config(&format!("{}/repos", server.uri()));
            config.version = current.to_owned();
            let service = ProcessSystemOperations::new(CancellationToken::new(), config);

            let detail = service.update_detail(true, None).await.expect("detail");
            assert_eq!(detail.has_update, allowed, "{current} -> {target}");
            assert_eq!(
                detail.latest_version,
                if allowed { target } else { current }
            );
            assert_eq!(detail.notes.is_some(), allowed);
            assert_eq!(detail.release_url.is_some(), allowed);
            assert!(detail.warning.is_none());
            let version = service.version().await.expect("version");
            assert_eq!(version.has_update, allowed);
            assert_eq!(version.update_channel, channel);
        }
    }
}

#[tokio::test]
async fn update_detail_should_keep_current_release_notes_without_allowing_reinstallation() {
    for current in [
        "3.12.1",
        "3.12.1-alpha.1",
        "3.12.1-beta.1",
        "3.12.1-rc.1",
        "3.12.1-exp.1",
        "3.12.1+build.1",
    ] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let release_url = format!("https://github.com/owner/repository/releases/tag/v{current}");
        Mock::given(method("GET"))
            .and(path("/repos/owner/repository/releases"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "tag_name": format!("v{current}"),
                    "prerelease": !semver::Version::parse(current).expect("version").pre.is_empty(),
                    "body": "## 当前版本\n\n- 修复更新状态",
                    "html_url": release_url,
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;
        let mut config = fixture.config(&format!("{}/repos", server.uri()));
        config.version = current.to_owned();
        if current.contains("-exp.") {
            config.build_type = "experimental".to_owned();
        }
        let service = ProcessSystemOperations::new(CancellationToken::new(), config);

        let detail = service
            .update_detail(true, None)
            .await
            .expect("current release");
        assert_eq!(detail.latest_version, current);
        assert!(!detail.has_update);
        assert!(detail.update_supported);
        assert_eq!(
            detail.notes.as_deref(),
            Some("## 当前版本\n\n- 修复更新状态")
        );
        assert_eq!(detail.release_url.as_deref(), Some(release_url.as_str()));
        let cached = service
            .update_detail(false, None)
            .await
            .expect("cached release");
        assert!(cached.cached);
        assert_eq!(cached.notes, detail.notes);
        assert_eq!(cached.release_url, detail.release_url);
        assert!(!service.version().await.expect("version").has_update);

        let error = service
            .perform_test_update(Some(current.to_owned()))
            .await
            .expect_err("current release must not be installed again");
        assert_eq!(error.kind(), SystemOperationErrorKind::Conflict);
        assert!(!fixture.state().exists());
        assert_eq!(
            fs::read(fixture.executable()).expect("binary"),
            b"old-binary"
        );
    }
}

#[tokio::test]
async fn update_detail_should_prefer_available_update_notes_over_current_release() {
    for current_first in [true, false] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let mut releases = vec![
            serde_json::json!({
                "tag_name": "v1.0.0", "prerelease": false, "body": "current notes"
            }),
            serde_json::json!({
                "tag_name": "v1.9.9", "prerelease": false, "body": "update notes",
                "html_url": "https://github.com/owner/repository/releases/tag/v1.9.9"
            }),
        ];
        if !current_first {
            releases.reverse();
        }
        Mock::given(method("GET"))
            .and(path("/repos/owner/repository/releases"))
            .respond_with(ResponseTemplate::new(200).set_body_json(releases))
            .mount(&server)
            .await;

        let detail = fixture
            .service(&server)
            .update_detail(true, None)
            .await
            .expect("detail");
        assert!(detail.has_update);
        assert_eq!(detail.latest_version, TARGET_VERSION);
        assert_eq!(detail.notes.as_deref(), Some("update notes"));
        assert_eq!(
            detail.release_url.as_deref(),
            Some("https://github.com/owner/repository/releases/tag/v1.9.9")
        );
    }
}

#[tokio::test]
async fn update_detail_should_find_current_release_notes_across_pages() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![
            serde_json::json!({ "tag_name": "v2.0.0", "prerelease": false });
            100
        ]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .and(query_param("page", "2"))
        .respond_with(release_response("1.0.0", Vec::new()))
        .expect(1)
        .mount(&server)
        .await;

    let detail = fixture
        .service(&server)
        .update_detail(true, None)
        .await
        .expect("detail");
    assert!(!detail.has_update);
    assert_eq!(detail.latest_version, "1.0.0");
    assert_eq!(detail.notes.as_deref(), Some("notes"));
    assert!(detail.release_url.is_some());
}

#[tokio::test]
async fn update_detail_should_not_use_draft_or_mislabeled_current_release_notes() {
    for (current, prerelease) in [("1.0.0", false), ("1.0.0-beta.1", true)] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        Mock::given(method("GET"))
            .and(path("/repos/owner/repository/releases"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "tag_name": format!("v{current}"), "prerelease": prerelease, "draft": true, "body": "draft notes" },
                { "tag_name": format!("v{current}"), "prerelease": !prerelease, "body": "mislabeled notes" },
                { "tag_name": "v0.9.0", "prerelease": false, "body": "older notes" },
            ])))
            .mount(&server)
            .await;
        let mut config = fixture.config(&format!("{}/repos", server.uri()));
        config.version = current.to_owned();
        let service = ProcessSystemOperations::new(CancellationToken::new(), config);

        let detail = service.update_detail(true, None).await.expect("detail");
        assert!(!detail.has_update);
        assert_eq!(detail.latest_version, current);
        assert!(detail.notes.is_none());
        assert!(detail.release_url.is_none());
    }
}

#[tokio::test]
async fn update_checks_should_ignore_downgrades_unknown_stages_and_build_metadata() {
    for (current, target) in [
        ("3.12.0-beta.2", "3.12.0-beta.1"),
        ("3.12.0", "3.12.0+new-build"),
        ("3.12.0-beta.1+aaa", "3.12.0-beta.1+zzz"),
        ("3.12.0", "3.11.0"),
        ("3.12.0-alpha.1", "3.12.0-preview.2"),
        ("3.12.0-alpha.1", "3.12.0-beta"),
        ("3.12.0-alpha.1", "3.12.0-beta.0"),
        ("3.12.0-alpha.1", "3.12.0-beta.2.extra"),
    ] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        fixture.mount_release_once(&server, target).await;
        let mut config = fixture.config(&format!("{}/repos", server.uri()));
        config.version = current.to_owned();
        let service = ProcessSystemOperations::new(CancellationToken::new(), config);
        let detail = service.update_detail(true, None).await.expect("detail");
        assert!(!detail.has_update, "{current} -> {target}");
        assert_eq!(detail.latest_version, current);
        assert!(detail.notes.is_none());
        assert!(detail.release_url.is_none());
    }
}

#[tokio::test]
async fn update_checks_should_fail_closed_for_unknown_current_channels() {
    for (current, build_type) in [
        ("3.12.0-preview.1", "release"),
        ("3.12.0-alpha", "release"),
        ("3.12.0-exp.0", "experimental"),
        ("invalid", "release"),
        ("3.12.0", "experimental"),
        ("3.12.0-beta.1", "experimental"),
    ] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let mut config = fixture.config(&format!("{}/repos", server.uri()));
        config.version = current.to_owned();
        config.build_type = build_type.to_owned();
        let service = ProcessSystemOperations::new(CancellationToken::new(), config);
        let detail = service.update_detail(true, None).await.expect("detail");
        assert!(!detail.has_update);
        assert!(!detail.update_supported);
        assert!(detail.unsupported_reason.is_some());
        assert!(
            server
                .received_requests()
                .await
                .expect("requests")
                .is_empty()
        );
    }
}

#[tokio::test]
async fn stable_update_should_select_allowed_version_below_newer_major_releases() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "tag_name": "v4.0.0", "prerelease": false },
            { "tag_name": "v3.13.0-beta.1", "prerelease": true },
            { "tag_name": "v3.12.1", "prerelease": true },
            { "tag_name": "v3.12.0", "prerelease": false, "draft": true },
            { "tag_name": "v3.11.2", "prerelease": false },
            { "tag_name": "v3.11.10", "prerelease": false, "body": "stable notes" }
        ])))
        .mount(&server)
        .await;
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    config.version = "3.11.0".to_owned();
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);
    let detail = service.update_detail(true, None).await.expect("detail");
    assert!(detail.has_update);
    assert_eq!(detail.latest_version, "3.11.10");
    assert_eq!(detail.notes.as_deref(), Some("stable notes"));
}

#[tokio::test]
async fn update_detail_should_use_cached_release_when_not_refreshed() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture.mount_release_once(&server, TARGET_VERSION).await;
    let service = fixture.service(&server);
    service
        .update_detail(true, None)
        .await
        .expect("prime cache");

    assert!(service.update_detail(false, None).await.is_ok());
}

#[tokio::test]
async fn update_detail_should_withhold_cross_major_release() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_release_once(&server, CROSS_MAJOR_VERSION)
        .await;

    let detail = fixture
        .service(&server)
        .update_detail(true, None)
        .await
        .expect("detail");
    assert_eq!(detail.latest_version, "1.0.0");
    assert!(!detail.has_update);
    assert!(detail.warning.is_none());
    assert!(detail.notes.is_none());
    assert!(detail.release_url.is_none());
}

#[tokio::test]
async fn version_reports_the_running_build_without_listing_releases() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(30))
                .set_body_json(serde_json::json!([
                    { "tag_name": "v1.1.0", "prerelease": false }
                ])),
        )
        .mount(&server)
        .await;
    let service = fixture.service(&server);

    let version = tokio::time::timeout(Duration::from_secs(1), service.version())
        .await
        .expect("running version must not wait for release discovery")
        .expect("version");

    assert_eq!(version.version, "1.0.0");
    assert_eq!(version.latest_version, "1.0.0");
    assert_eq!(version.update_channel, "stable");
    assert!(!version.has_update);
    assert!(!version.update_cached);
    assert!(version.update_warning.is_none());
    assert!(
        server
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
}
