//! 验证额度预热的窗口判断、SSE 终态与凭据边界

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
                quota_pause: None,
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
        ("acct_zero_full", 0, Some(18_000), 18_000, false, true),
        ("acct_zero_near_full", 0, Some(17_999), 17_999, false, true),
        ("acct_zero_lower", 0, Some(12_000), 12_000, false, true),
        ("acct_zero_missing_countdown", 0, None, 14_400, false, true),
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
async fn warmup_skips_exhausted_account_windows_without_refreshing_quota() {
    for exhausted in [true, false] {
        let server = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_exhausted").await;
        let account = store.account("acct_exhausted").unwrap();
        let now = SystemTime::now();
        let quota = json!({"rate_limit": {"allowed": true, "secondary_window": {
            "used_percent": 100, "limit_window_seconds": 604_800,
            "reset_at": Utc::now().timestamp() + 604_800,
        }}});
        store
            .compare_and_swap_quota(QuotaObservation {
                quota_pause: None,
                expected_connection: None,
                plan_type: None,
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                quota: OpaqueProviderData::new(quota.as_object().unwrap().clone()),
                observed_at: now,
                state: if exhausted {
                    QuotaState::exhausted(QuotaEvidence::UsageLimitReached, now, None)
                } else {
                    QuotaState::allowed(now)
                },
            })
            .await
            .unwrap();
        let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
        let summary = service.execute_warmup("test-model").await.unwrap();
        assert_eq!(summary.skipped_exhausted, 1);
        assert_eq!(summary.warmed_up, 0);
        assert_eq!(summary.failed, 0);
        assert!(server.received_requests().await.unwrap().is_empty());
    }
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
async fn warmup_uses_global_profile_and_lowest_reasoning_effort() {
    let server = MockServer::start().await;
    let sse = concat!(
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_warmup_effort\",\"status\":\"completed\",\"output\":[]}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(wiremock::matchers::header("version", "0.169.0"))
        .and(wiremock::matchers::header("originator", "codex_cli_rs"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_warmup_effort").await;
    let policy = Arc::new(crate::support::RequestProfileRuntimePolicy(std::sync::Mutex::new(
        OpaqueProviderData::new(json!({"client":"cli","platform":"macos","versionMode":"fixed","codexVersion":"0.169.0"}).as_object().unwrap().clone()),
    )));
    let service = CodexCredentialQuotaService::new(
        store.repository(),
        wire_profile(),
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
        Arc::new(crate::support::MemoryCooldownPort::new()),
        policy.clone(),
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
    *policy.0.lock().unwrap() = OpaqueProviderData::new(
        json!({"mode":"custom","userAgent":"invalid"})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert!(matches!(
        service.execute_warmup("test-model").await,
        Err(CodexCredentialQuotaError::Profile(_))
    ));
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
