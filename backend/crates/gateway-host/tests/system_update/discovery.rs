//! 用本地 HTTP 源验证请求预算、限流隔离与执行前复核

use super::*;
use gateway_admin::model::system::SystemUpdateChannel;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn discovery_request_budget_is_constant_under_load() {
    for history in [1, 100, 101, 10_000] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let service = Arc::new(fixture.service(&server));
        let target = format!("1.1.{history}");
        let releases: Vec<_> = (0..history)
            .map(|i| {
                let tag = if i == 0 {
                    format!("v{target}")
                } else {
                    format!("v1.0.{i}")
                };
                serde_json::json!({"tag_name":tag, "draft":false, "prerelease":false})
            })
            .collect();
        Mock::given(path("/repos/owner/repository/releases"))
            .respond_with(move |req: &wiremock::Request| {
                let page = req
                    .url
                    .query_pairs()
                    .find(|(k, _)| k == "page")
                    .and_then(|(_, v)| v.parse::<usize>().ok())
                    .unwrap_or(1);
                ResponseTemplate::new(200).set_body_json(
                    releases
                        .iter()
                        .skip(page.saturating_sub(1) * 100)
                        .take(100)
                        .collect::<Vec<_>>(),
                )
            })
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(path(INDEX_PATH))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(fixture_index(&target))
                    .set_delay(Duration::from_millis(80)),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path(format!(
            "/repos/owner/repository/releases/tags/v{target}"
        )))
        .respond_with(release_response(&target, vec![]).set_delay(Duration::from_millis(30)))
        .expect(1)
        .mount(&server)
        .await;
        let started = std::time::Instant::now();
        assert!(!service.version().await.unwrap().update_cached);
        assert!(server.received_requests().await.unwrap().is_empty());
        let summary = service
            .update_detail(SystemUpdateCheck::Summary, None)
            .await
            .unwrap();
        assert_eq!(summary.latest_version, target);
        assert!(summary.notes.is_none());
        assert_eq!(server.received_requests().await.unwrap().len(), 1);

        let mut callers = tokio::task::JoinSet::new();
        for i in 0..512 {
            let service = service.clone();
            callers.spawn(async move {
                let check = if i % 2 == 0 {
                    SystemUpdateCheck::Refresh
                } else {
                    SystemUpdateCheck::Detail
                };
                service.update_detail(check, None).await.unwrap()
            });
        }
        while let Some(detail) = callers.join_next().await {
            let detail = detail.unwrap();
            assert_eq!(detail.latest_version, target);
            assert_eq!(detail.notes.as_deref(), Some("notes"));
        }
        for _ in 0..64 {
            assert!(service.version().await.unwrap().has_update);
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        eprintln!(
            "history={history} concurrent_detail=512 local_probes=65 raw=1 rest=1 lists=0 elapsed={:?}",
            started.elapsed()
        );
    }
}

#[tokio::test]
async fn cold_summary_stampede_and_slow_source_do_not_block_runtime_probes() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let service = Arc::new(fixture.service(&server));
    let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
    Mock::given(path(INDEX_PATH))
        .respond_with(move |_: &wiremock::Request| {
            started.send(()).unwrap();
            ResponseTemplate::new(200)
                .set_body_json(fixture_index(TARGET_VERSION))
                .set_delay(Duration::from_millis(500))
        })
        .expect(1)
        .mount(&server)
        .await;
    let mut callers = tokio::task::JoinSet::new();
    for _ in 0..256 {
        let service = service.clone();
        callers.spawn(async move {
            service
                .update_detail(SystemUpdateCheck::Summary, None)
                .await
                .unwrap()
        });
    }
    observed.recv().await.unwrap();
    tokio::time::timeout(Duration::from_millis(150), async {
        for _ in 0..128 {
            let version = service.version().await.unwrap();
            assert_eq!(version.version, "1.0.0");
            assert!(!version.update_cached);
        }
    })
    .await
    .expect("本地版本探测不能等待在途的 GitHub 查询");
    while let Some(summary) = callers.join_next().await {
        assert!(summary.unwrap().has_update);
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn api_cooldown_is_shared_across_channels_and_does_not_block_raw_summary() {
    for status in [403, 429] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let service = fixture.service(&server);
        mount_index(
            &server,
            serde_json::json!({"published":{"1":{"stable":"1.1.0","beta":"1.2.0-beta.1"}}}),
        )
        .await;
        let limit = if status == 429 {
            ResponseTemplate::new(status).insert_header("retry-after", "300")
        } else {
            let reset = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 300;
            ResponseTemplate::new(status)
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset", reset.to_string())
        };
        Mock::given(path("/repos/owner/repository/releases/tags/v1.1.0"))
            .respond_with(limit)
            .expect(1)
            .mount(&server)
            .await;
        let first = service
            .update_detail(SystemUpdateCheck::Refresh, None)
            .await
            .unwrap();
        assert!(first.warning.is_some());
        let retry_at = first.retry_at.expect("限流应提供重试时间");
        assert!((298..=300).contains(&(retry_at - chrono::Utc::now()).num_seconds()));
        for _ in 0..32 {
            let second = service
                .update_detail(SystemUpdateCheck::Refresh, Some(SystemUpdateChannel::Beta))
                .await
                .unwrap();
            assert_eq!(second.warning, first.warning);
            assert_eq!(second.retry_at, Some(retry_at));
            assert!(!second.has_update);
        }
        assert!(
            service
                .update_detail(SystemUpdateCheck::Summary, None)
                .await
                .unwrap()
                .has_update
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
}

#[tokio::test]
async fn invalid_or_missing_published_data_does_not_fallback_to_build_version_or_lists() {
    for body in [
        "version: '1.9.9'".to_owned(),
        "version: '9.0.0'\npublished: {'2': {stable: '2.0.0'}}".to_owned(),
        "published: {'1': {stable: '2.0.0'}}".to_owned(),
        "published: {'1': {stable: '1.2.0-rc.1'}}".to_owned(),
        "bad: &loop [1]\npublished: *loop".to_owned(),
        "x".repeat(8193),
    ] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let service = fixture.service(&server);
        Mock::given(path(INDEX_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .expect(1)
            .mount(&server)
            .await;
        for _ in 0..2 {
            let detail = service
                .update_detail(SystemUpdateCheck::Refresh, None)
                .await
                .unwrap();
            assert!(detail.warning.is_some());
            assert!(!detail.has_update);
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn exact_release_requires_public_matching_identity_before_display() {
    for (status, tag, draft, prerelease) in [
        (404, "", false, false),
        (200, "v1.2.0", false, false),
        (200, "v1.1.0", true, false),
        (200, "v1.1.0", false, true),
    ] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let service = fixture.service(&server);
        mount_index(&server, fixture_index("1.1.0")).await;
        Mock::given(path("/repos/owner/repository/releases/tags/v1.1.0"))
            .respond_with(ResponseTemplate::new(status).set_body_json(
                serde_json::json!({"tag_name":tag,"draft":draft,"prerelease":prerelease}),
            ))
            .mount(&server)
            .await;
        let detail = service
            .update_detail(SystemUpdateCheck::Detail, None)
            .await
            .unwrap();
        assert!(!detail.has_update);
        assert!(detail.warning.is_some());
    }
}

#[tokio::test]
async fn install_rechecks_even_a_warm_candidate_and_never_downloads_a_changed_target() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let service = fixture.service(&server);
    let requests = AtomicUsize::new(0);
    Mock::given(path(INDEX_PATH))
        .respond_with(move |_: &wiremock::Request| {
            let v = if requests.fetch_add(1, Ordering::SeqCst) == 0 {
                "1.1.0"
            } else {
                "1.2.0"
            };
            ResponseTemplate::new(200).set_body_json(fixture_index(v))
        })
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(path("/repos/owner/repository/releases/tags/v1.1.0"))
        .respond_with(release_response("1.1.0", vec![]))
        .expect(1)
        .mount(&server)
        .await;
    let detail = service
        .update_detail(SystemUpdateCheck::Detail, None)
        .await
        .unwrap();
    assert!(detail.has_update);
    let status = complete_update(&service, &detail.latest_version).await;
    assert_eq!(status.operation.status, SystemOperationStatus::Failed);
    assert!(status.operation.error.unwrap().contains("重新检查并确认"));
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn channels_pointing_to_one_release_share_its_detail() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let service = fixture.service(&server);
    mount_release_json(&server, TARGET_VERSION, vec![], None).await;
    for channel in [
        SystemUpdateChannel::Stable,
        SystemUpdateChannel::Rc,
        SystemUpdateChannel::Beta,
        SystemUpdateChannel::Alpha,
    ] {
        assert!(
            service
                .update_detail(SystemUpdateCheck::Refresh, Some(channel))
                .await
                .unwrap()
                .has_update
        );
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn transient_release_absence_expires_and_refresh_interval_is_bounded() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let service = fixture.service(&server);
    Mock::given(path(INDEX_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture_index(TARGET_VERSION)))
        .expect(2)
        .mount(&server)
        .await;
    let calls = AtomicUsize::new(0);
    Mock::given(path(format!(
        "/repos/owner/repository/releases/tags/v{TARGET_VERSION}"
    )))
    .respond_with(move |_: &wiremock::Request| {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            ResponseTemplate::new(404)
        } else {
            release_response(TARGET_VERSION, vec![])
        }
    })
    .expect(3)
    .mount(&server)
    .await;
    assert!(
        service
            .update_detail(SystemUpdateCheck::Detail, None)
            .await
            .unwrap()
            .warning
            .is_some()
    );
    assert!(
        service
            .update_detail(SystemUpdateCheck::Refresh, None)
            .await
            .unwrap()
            .warning
            .is_some()
    );
    // 只推进本进程缓存时钟，实际 HTTP 连接使用正常计时
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::resume();
    assert!(
        service
            .update_detail(SystemUpdateCheck::Detail, None)
            .await
            .unwrap()
            .has_update
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::resume();
    assert!(
        service
            .update_detail(SystemUpdateCheck::Detail, None)
            .await
            .unwrap()
            .cached
    );
    assert!(
        !service
            .update_detail(SystemUpdateCheck::Refresh, None)
            .await
            .unwrap()
            .cached
    );
    assert!(
        service
            .update_detail(SystemUpdateCheck::Refresh, None)
            .await
            .unwrap()
            .cached
    );
}

#[tokio::test]
async fn metadata_never_follows_redirects_or_accepts_oversized_release() {
    for status in [302, 200] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let service = fixture.service(&server);
        mount_index(&server, fixture_index(TARGET_VERSION)).await;
        let response = if status == 302 {
            ResponseTemplate::new(status)
                .insert_header("location", format!("{}/untrusted", server.uri()))
        } else {
            ResponseTemplate::new(200).set_body_bytes(vec![b' '; 4 * 1024 * 1024 + 1])
        };
        Mock::given(path(format!(
            "/repos/owner/repository/releases/tags/v{TARGET_VERSION}"
        )))
        .respond_with(response)
        .expect(1)
        .mount(&server)
        .await;
        let detail = service
            .update_detail(SystemUpdateCheck::Detail, None)
            .await
            .unwrap();
        assert!(!detail.has_update);
        assert!(detail.warning.unwrap().contains(if status == 302 {
            "HTTP 302"
        } else {
            "大小限制"
        }));
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
}

#[tokio::test]
async fn slow_metadata_times_out_without_retry_or_blocking_future_version_reads() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let service = Arc::new(fixture.service(&server));
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    Mock::given(path(INDEX_PATH))
        .respond_with(move |_: &wiremock::Request| {
            sent.send(()).unwrap();
            ResponseTemplate::new(200).set_delay(Duration::from_secs(30))
        })
        .expect(1)
        .mount(&server)
        .await;
    let worker = Arc::clone(&service);
    let pending = tokio::spawn(async move {
        worker
            .update_detail(SystemUpdateCheck::Summary, None)
            .await
            .unwrap()
    });
    received.recv().await.unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(11)).await;
    let detail = pending.await.unwrap();
    tokio::time::resume();
    assert!(detail.warning.is_some());
    assert!(!detail.has_update);
    assert_eq!(service.version().await.unwrap().version, "1.0.0");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}
