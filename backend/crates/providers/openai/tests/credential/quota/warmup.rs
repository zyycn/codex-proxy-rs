//! 验证额度预热的窗口识别、实时复核、SSE 终态与凭据边界

use super::*;
use gateway_core::account::{OpaqueProviderData, QuotaObservation, QuotaWriteOutcome};

fn short_window(used: u64, remaining: Option<u64>, reset: i64) -> serde_json::Value {
    let mut window = json!({
        "used_percent": used,
        "limit_window_seconds": 18_000,
        "reset_at": reset,
    });
    if let Some(remaining) = remaining {
        window["reset_after_seconds"] = json!(remaining);
    }
    window
}

fn usage(window: serde_json::Value) -> serde_json::Value {
    json!({"rate_limit": {"allowed": true, "primary_window": window}})
}

async fn seed_quota(
    store: &MemoryAccountStore,
    account_id: &str,
    quota: serde_json::Value,
    observed_at: SystemTime,
) {
    let account = store.account(account_id).expect("account");
    assert_eq!(
        store
            .compare_and_swap_quota(QuotaObservation {
                expected_connection: None,
                plan_type: None,
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                quota: OpaqueProviderData::new(quota.as_object().unwrap().clone()),
                observed_at,
                state: QuotaState::allowed(observed_at),
            })
            .await
            .expect("persist quota"),
        QuotaWriteOutcome::Updated
    );
}

async fn mount_completion(server: &MockServer, expected: u64) {
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
            "text/event-stream",
        ))
        .expect(expected)
        .mount(server)
        .await;
}

#[tokio::test]
async fn warmup_uses_account_windows_and_preserves_the_reset_threshold() {
    for (name, used, remaining, until_reset, named, warmed) in [
        ("acct_zero_active", 0, Some(17_999), 17_999, false, false),
        ("acct_passive_active", 1, None, 14_400, false, false),
        ("acct_near_reset", 1, Some(1_800), 1_800, false, true),
        ("acct_expired", 1, Some(3_600), -1, false, true),
        ("acct_reset_zero", 0, Some(0), -1, false, true),
        ("acct_named_active", 1, Some(3_600), 3_600, true, true),
    ] {
        let server = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, name).await;
        let window = short_window(used, remaining, Utc::now().timestamp() + until_reset);
        let quota = if named {
            json!({
                "rate_limit": {"allowed": true},
                "additional_rate_limits": [{
                    "limit_id": "other_feature",
                    "rate_limit": {
                        "primary_window": window,
                        "secondary_window": {
                            "used_percent": 100,
                            "limit_window_seconds": 604_800,
                            "reset_at": Utc::now().timestamp() + 604_800,
                        },
                    },
                }],
            })
        } else {
            usage(window)
        };
        seed_quota(&store, name, quota, SystemTime::now()).await;
        mount_completion(&server, u64::from(warmed)).await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());

        let summary = service.execute_warmup("test-model").await.unwrap();

        assert_eq!(
            (
                summary.warmed_up,
                summary.skipped_active,
                summary.skipped_exhausted,
                summary.failed
            ),
            (u64::from(warmed), u64::from(!warmed), 0, 0),
            "{name}"
        );
    }
}

#[tokio::test]
async fn warmup_confirms_idle_full_windows_even_when_the_saved_snapshot_is_old() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_idle").await;
    seed_quota(
        &store,
        "acct_idle",
        usage(short_window(
            0,
            Some(18_000),
            Utc::now().timestamp() + 14_400,
        )),
        SystemTime::now() - Duration::from_secs(3_600),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(|_: &wiremock::Request| {
            let mut quota = usage(short_window(
                0,
                Some(18_000),
                Utc::now().timestamp() + 18_000,
            ));
            quota["plan_type"] = json!("team");
            ResponseTemplate::new(200).set_body_json(quota)
        })
        .expect(2)
        .mount(&server)
        .await;
    mount_completion(&server, 1).await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());

    let summary = service.execute_warmup("test-model").await.unwrap();

    assert_eq!(
        (summary.warmed_up, summary.skipped_active, summary.failed),
        (1, 0, 0)
    );
}

#[tokio::test]
async fn warmup_refreshes_zero_percent_passive_windows_before_skipping() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_passive_zero").await;
    let reset = Utc::now().timestamp() + 14_400;
    seed_quota(
        &store,
        "acct_passive_zero",
        usage(short_window(0, None, reset)),
        SystemTime::now(),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(usage(short_window(
            0,
            Some(14_400),
            reset,
        ))))
        .expect(1)
        .mount(&server)
        .await;
    mount_completion(&server, 0).await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());

    let summary = service.execute_warmup("test-model").await.unwrap();

    assert_eq!(
        (summary.warmed_up, summary.skipped_active, summary.failed),
        (0, 1, 0)
    );
}

#[tokio::test]
async fn warmup_rechecks_a_just_activated_full_zero_percent_window() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_just_active").await;
    let reset = Utc::now().timestamp() + 18_000;
    let full = usage(short_window(0, Some(18_000), reset));
    seed_quota(&store, "acct_just_active", full.clone(), SystemTime::now()).await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(full))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(usage(short_window(
            0,
            Some(17_998),
            reset,
        ))))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    mount_completion(&server, 0).await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());

    let summary = service.execute_warmup("test-model").await.unwrap();

    assert_eq!(
        (summary.warmed_up, summary.skipped_active, summary.failed),
        (0, 1, 0)
    );
}

#[tokio::test]
async fn warmup_quota_verification_failure_does_not_abort_other_accounts() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_a_uncertain").await;
    create_account(&store, "acct_b_ready").await;
    seed_quota(
        &store,
        "acct_a_uncertain",
        usage(short_window(0, None, Utc::now().timestamp() + 14_400)),
        SystemTime::now(),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(400))
        .expect(1)
        .mount(&server)
        .await;
    mount_completion(&server, 1).await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());

    let summary = service.execute_warmup("test-model").await.unwrap();

    assert_eq!(
        (summary.warmed_up, summary.skipped_active, summary.failed),
        (1, 0, 1)
    );
}

#[tokio::test]
async fn warmup_respects_exhaustion_discovered_by_quota_verification() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_exhausted").await;
    let reset = Utc::now().timestamp() + 14_400;
    seed_quota(
        &store,
        "acct_exhausted",
        usage(short_window(0, None, reset)),
        SystemTime::now(),
    )
    .await;
    let mut exhausted = usage(short_window(100, Some(14_400), reset));
    exhausted["rate_limit"]["allowed"] = json!(false);
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(exhausted))
        .expect(1)
        .mount(&server)
        .await;
    mount_completion(&server, 0).await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());

    let summary = service.execute_warmup("test-model").await.unwrap();

    assert_eq!(
        (summary.warmed_up, summary.skipped_exhausted, summary.failed),
        (0, 1, 0)
    );
}

#[tokio::test]
async fn warmup_bounds_quota_verification_when_activity_remains_unknown() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_unknown").await;
    let quota = usage(short_window(0, None, Utc::now().timestamp() + 14_400));
    seed_quota(&store, "acct_unknown", quota.clone(), SystemTime::now()).await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(quota))
        .expect(2)
        .mount(&server)
        .await;
    mount_completion(&server, 0).await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());

    let summary = service.execute_warmup("test-model").await.unwrap();

    assert_eq!(
        (summary.warmed_up, summary.skipped_active, summary.failed),
        (0, 0, 1)
    );
}

#[tokio::test]
async fn warmup_consumes_completed_sse_and_persists_rate_limit_event() {
    let server = MockServer::start().await;
    let sse = concat!(
        "event: codex.rate_limits\n",
        "data: {\"type\":\"codex.rate_limits\",\"plan_type\":\"team\",\"rate_limits\":{\"allowed\":true,\"limit_reached\":false,\"primary\":{\"used_percent\":5,\"window_minutes\":300,\"reset_at\":1900000000}}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_warmup\",\"status\":\"completed\",\"output\":[]}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_warmup_completed").await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
    );

    let summary = service.execute_warmup("test-model").await.expect("warmup");

    assert_eq!(summary.warmed_up, 1);
    assert_eq!(summary.failed, 0);
    assert_eq!(
        store.account("acct_warmup_completed").unwrap().plan_type(),
        Some("team")
    );
    assert!(store.quota_json("acct_warmup_completed").is_some());
}

#[tokio::test]
async fn warmup_sends_the_lowest_reasoning_effort_supported_by_the_catalog() {
    let server = MockServer::start().await;
    let sse = concat!(
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_warmup_effort\",\"status\":\"completed\",\"output\":[]}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_warmup_effort").await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
    );

    let summary = service.execute_warmup("test-model").await.expect("warmup");

    assert_eq!(summary.warmed_up, 1);
    let requests = server.received_requests().await.expect("recorded requests");
    let decoded =
        zstd::stream::decode_all(requests[0].body.as_slice()).expect("zstd body should decode");
    let body: serde_json::Value =
        serde_json::from_slice(&decoded).expect("warmup body should be valid JSON");
    // 上游按目录拒绝目录外的档位，none 曾让全部定时预热只留下 error 终态
    assert_eq!(body["reasoning"]["effort"], "low");
}

#[tokio::test]
async fn warmup_rejects_failed_sse_despite_successful_http_headers() {
    let server = MockServer::start().await;
    let sse = concat!(
        "event: codex.rate_limits\n",
        "data: {\"type\":\"codex.rate_limits\",\"plan_type\":\"team\",\"rate_limits\":{\"allowed\":false,\"limit_reached\":true,\"primary\":{\"used_percent\":100,\"window_minutes\":300,\"reset_at\":1900000000}}}\n\n",
        "event: response.failed\n",
        "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\"}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-codex-allowed", "false")
                .set_body_raw(sse, "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_warmup_failed").await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
    );

    let summary = service.execute_warmup("test-model").await.expect("warmup");

    assert_eq!(summary.warmed_up, 0);
    assert_eq!(summary.failed, 1);
    assert!(store.quota_json("acct_warmup_failed").is_none());
}

#[tokio::test]
async fn warmup_skips_expired_credential() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_warmup_expired").await;
    let account = store.account("acct_warmup_expired").expect("account");
    persist_credential_state(&store, &account, CredentialState::Expired).await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
    );

    let summary = service.execute_warmup("test-model").await.expect("warmup");

    assert_eq!(summary.warmed_up, 0);
    assert_eq!(summary.failed, 0);
}
