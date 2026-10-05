//! 验证更新通道选择、发行候选缓存与并发查询结果隔离

use super::*;
use gateway_admin::model::system::SystemUpdateChannel::{Alpha, Beta, Experimental, Rc, Stable};

#[tokio::test]
async fn temporary_channel_should_filter_future_releases_without_changing_running_channel() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let releases: Vec<_> = ["1.1.0", "1.2.0-rc.1", "1.3.0-beta.1", "1.4.0-alpha.1", "1.9.0-exp.9", "2.0.0"]
        .into_iter().map(|version| serde_json::json!({"tag_name": format!("v{version}"), "prerelease": version.contains('-')})).collect();
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(releases))
        .mount(&server)
        .await;
    let service = fixture.service(&server);
    for (channel, target) in [
        (Stable, "1.1.0"),
        (Rc, "1.2.0-rc.1"),
        (Beta, "1.3.0-beta.1"),
        (Alpha, "1.4.0-alpha.1"),
        (Stable, "1.1.0"),
    ] {
        let detail = service
            .update_detail(false, Some(channel))
            .await
            .expect("detail");
        assert_eq!(detail.latest_version, target);
        assert_eq!(detail.policy.channel, channel);
        assert_eq!(
            detail.policy.available_channels,
            vec![Stable, Rc, Beta, Alpha]
        );
        assert!(detail.has_update);
        let cached = service
            .update_detail(false, Some(channel))
            .await
            .expect("cached");
        assert!(cached.cached);
        assert_eq!(cached.latest_version, target);
    }
    let version = service.version().await.expect("default check");
    assert_eq!(version.update_channel, "stable");
    assert_eq!(version.latest_version, "1.1.0");
    assert!(!fixture.state().exists(), "检查通道不能写入实例偏好");
    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 5);
    assert!(
        requests
            .iter()
            .all(|request| request.url.path().ends_with("/releases"))
    );
}

#[tokio::test]
async fn reopening_should_infer_running_channel_and_switching_to_stable_should_not_downgrade() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .respond_with(release_response("1.0.0", Vec::new()))
        .mount(&server)
        .await;
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    config.version = "1.1.0-beta.1".into();
    let service = ProcessSystemOperations::new(CancellationToken::new(), config.clone());
    let temporary = service
        .update_detail(true, Some(Stable))
        .await
        .expect("view stable");
    assert_eq!(temporary.policy.channel, Stable);
    assert!(!temporary.has_update);
    assert_eq!(temporary.latest_version, "1.1.0-beta.1");
    assert_eq!(
        service
            .update_detail(false, None)
            .await
            .expect("reopened")
            .policy
            .channel,
        Beta
    );
    let restarted = ProcessSystemOperations::new(CancellationToken::new(), config.clone());
    assert_eq!(
        restarted
            .update_detail(false, None)
            .await
            .expect("restarted")
            .policy
            .channel,
        Beta
    );
    config.version = "1.1.0".into();
    let promoted = ProcessSystemOperations::new(CancellationToken::new(), config);
    assert_eq!(
        promoted
            .update_detail(false, None)
            .await
            .expect("now stable")
            .policy
            .channel,
        Stable
    );
    assert!(!fixture.state().exists());
}

#[tokio::test]
async fn install_should_use_confirmed_channel_and_infer_new_default_only_after_restart() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_release(
            &server,
            "1.1.0-beta.1",
            ArchiveKind::Safe,
            ChecksumKind::Valid,
        )
        .await;
    let service = fixture.service(&server);
    let error = service
        .perform_update(
            Some("1.1.0-beta.1".into()),
            Some(Stable),
            Arc::new(AllowingUpdatePreflight),
        )
        .await
        .expect_err("target outside confirmed channel");
    assert_eq!(error.kind(), SystemOperationErrorKind::Conflict);
    assert!(
        server
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
    service
        .perform_update(
            Some("1.1.0-beta.1".into()),
            Some(Beta),
            Arc::new(AllowingUpdatePreflight),
        )
        .await
        .expect("accept selected target");
    let status = wait_for_update(&service).await;
    assert_eq!(status.operation.status, SystemOperationStatus::Succeeded);
    assert!(status.need_restart);
    assert_eq!(
        service
            .version()
            .await
            .expect("still running stable")
            .update_channel,
        "stable"
    );
    assert!(
        service
            .perform_update(
                Some("1.1.0-beta.1".into()),
                Some(Alpha),
                Arc::new(AllowingUpdatePreflight)
            )
            .await
            .is_err()
    );
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    config.version = "1.1.0-beta.1".into();
    let restarted = ProcessSystemOperations::new(CancellationToken::new(), config);
    assert!(
        !restarted
            .update_status()
            .await
            .expect("restarted")
            .need_restart
    );
    assert_eq!(
        restarted
            .update_detail(false, None)
            .await
            .expect("new default")
            .policy
            .channel,
        Beta
    );
}

#[tokio::test]
async fn temporary_channels_should_keep_experimental_isolation() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let service = fixture.service(&server);
    assert!(
        service
            .update_detail(true, Some(Experimental))
            .await
            .is_err()
    );
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    config.version = "1.1.0-exp.1".into();
    config.build_type = "experimental".into();
    let experimental = ProcessSystemOperations::new(CancellationToken::new(), config);
    for channel in [Stable, Rc, Beta, Alpha] {
        assert!(
            experimental
                .update_detail(true, Some(channel))
                .await
                .is_err()
        );
    }
    assert!(
        server
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
}

#[tokio::test]
async fn slower_check_should_not_overwrite_newer_cached_candidate() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .respond_with(release_response("1.1.0", Vec::new()).set_delay(Duration::from_millis(150)))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .respond_with(release_response("1.2.0", Vec::new()))
        .mount(&server)
        .await;
    let service = fixture.service(&server);
    let first_service = service.clone();
    let first = tokio::spawn(async move { first_service.update_detail(true, None).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while server
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first request sent");
    assert_eq!(
        service
            .update_detail(true, None)
            .await
            .expect("new check")
            .latest_version,
        "1.2.0"
    );
    assert_eq!(
        first
            .await
            .expect("join")
            .expect("old check")
            .latest_version,
        "1.1.0"
    );
    let cached = service.update_detail(false, None).await.expect("cache");
    assert!(cached.cached);
    assert_eq!(cached.latest_version, "1.2.0");
}

#[tokio::test]
async fn inferred_prerelease_policy_should_follow_future_minor_and_patch_releases() {
    for (current, target) in [
        ("3.12.0-alpha.1", "3.13.0-alpha.2"),
        ("3.12.0-beta.1", "3.12.1-rc.1"),
        ("3.12.0-rc.1", "3.13.0"),
    ] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        fixture
            .mount_release(&server, target, ArchiveKind::Safe, ChecksumKind::Valid)
            .await;
        let mut config = fixture.config(&format!("{}/repos", server.uri()));
        config.version = current.to_owned();
        let service = ProcessSystemOperations::new(CancellationToken::new(), config);
        let detail = service.update_detail(true, None).await.expect("check");
        assert!(detail.has_update);
        assert_eq!(detail.latest_version, target);
        assert_eq!(
            complete_update(&service, target).await.operation.status,
            SystemOperationStatus::Succeeded
        );
    }
}
