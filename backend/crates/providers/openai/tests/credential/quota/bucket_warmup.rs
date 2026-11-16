//! 验证通用额度桶延迟预热的时间边界、使用证据和重启去重

use super::*;
use gateway_core::account::{OpaqueProviderData, QuotaObservation};

async fn setup() -> (
    Arc<MemoryAccountStore>,
    MockServer,
    chrono::DateTime<Utc>,
    i64,
) {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_bucket_warmup").await;
    store.set_quota_settings(
        "acct_bucket_warmup",
        serde_json::from_value(json!({
            "codex:604800s": {"warmup": {"model": "test-model"}}
        }))
        .unwrap(),
    );
    let now = Utc::now();
    let reset = now.timestamp() - 300;
    let account = store.account("acct_bucket_warmup").unwrap();
    let observed_at = SystemTime::from(Utc.timestamp_opt(reset - 60, 0).unwrap());
    store
        .compare_and_swap_quota(QuotaObservation {
            quota_pause: None,
            expected_connection: None,
            plan_type: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: OpaqueProviderData::new(usage(reset, 0).as_object().unwrap().clone()),
            observed_at,
            state: QuotaState::allowed(observed_at),
        })
        .await
        .unwrap();
    (store, MockServer::start().await, now, reset)
}

fn usage(reset: i64, used: u64) -> serde_json::Value {
    json!({"rate_limit": {"allowed": true, "secondary_window": {
        "used_percent": used, "limit_window_seconds": 604800, "reset_at": reset,
    }}})
}

fn service(store: &Arc<MemoryAccountStore>, server: &MockServer) -> CodexCredentialQuotaService {
    quota_service_with_base_url(
        store,
        reqwest::Client::builder().no_proxy().build().unwrap(),
        server.uri(),
    )
}

async fn observe_request(
    service: &CodexCredentialQuotaService,
    store: &MemoryAccountStore,
    reset: i64,
    named: bool,
) {
    let account = store.account("acct_bucket_warmup").unwrap();
    let window = json!({"used_percent": 0, "window_minutes": 10080, "reset_at": reset});
    let event = if named {
        json!({"type":"codex.rate_limits", "rate_limits": {"primary": window},
            "additional_rate_limits": {"Other feature": {"primary": window}}})
    } else {
        json!({"type":"codex.rate_limits", "rate_limits": {"secondary": window}})
    };
    let observation = parse_rate_limits_event(&event).unwrap();
    assert!(
        service
            .synchronize_passive_rate_limits(&account, &[observation])
            .await
            .unwrap()
    );
}

async fn mount_usage(server: &MockServer, reset: i64, used: u64, expected: u64) {
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(usage(reset, used)))
        .expect(expected)
        .mount(server)
        .await;
}

async fn mount_warmup(server: &MockServer, expected: u64) {
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
            "text/event-stream",
        ))
        .expect(expected)
        .mount(server)
        .await;
}

#[tokio::test]
async fn waits_five_minutes_and_claim_survives_service_restart() {
    let (store, server, now, reset) = setup().await;
    let quota = service(&store, &server);
    mount_usage(&server, reset + 604800, 0, 1).await;
    mount_warmup(&server, 1).await;
    let before = Utc.timestamp_opt(reset + 299, 0).unwrap();
    assert_eq!(
        quota
            .execute_bucket_warmup_at(before)
            .await
            .unwrap()
            .warmed_up,
        0
    );
    assert!(server.received_requests().await.unwrap().is_empty());
    let (one, two) = tokio::join!(
        quota.execute_bucket_warmup_at(now),
        quota.execute_bucket_warmup_at(now)
    );
    assert_eq!(one.unwrap().warmed_up + two.unwrap().warmed_up, 1);
    let restarted = service(&store, &server);
    assert_eq!(
        restarted
            .execute_bucket_warmup_at(now + chrono::Duration::minutes(10))
            .await
            .unwrap()
            .warmed_up,
        0
    );
}

#[tokio::test]
async fn zero_percent_request_skips_cycle_and_survives_usage_refresh() {
    let (store, server, now, reset) = setup().await;
    let quota = service(&store, &server);
    quota
        .execute_bucket_warmup_at(Utc.timestamp_opt(reset - 1, 0).unwrap())
        .await
        .unwrap();
    observe_request(&quota, &store, reset + 604800, false).await;
    mount_usage(&server, reset + 604800, 0, 1).await;
    mount_warmup(&server, 0).await;
    quota
        .refresh_account(&store.account("acct_bucket_warmup").unwrap().id().clone())
        .await
        .unwrap();
    assert_eq!(
        quota
            .execute_bucket_warmup_at(now + chrono::Duration::minutes(10))
            .await
            .unwrap()
            .warmed_up,
        0
    );
    assert_eq!(
        service(&store, &server)
            .execute_bucket_warmup_at(now + chrono::Duration::hours(1))
            .await
            .unwrap()
            .warmed_up,
        0
    );
}

#[tokio::test]
async fn named_bucket_request_does_not_skip_general_bucket() {
    let (store, server, now, reset) = setup().await;
    let quota = service(&store, &server);
    quota
        .execute_bucket_warmup_at(Utc.timestamp_opt(reset - 1, 0).unwrap())
        .await
        .unwrap();
    observe_request(&quota, &store, reset + 604800, true).await;
    mount_usage(&server, reset + 604800, 0, 1).await;
    mount_warmup(&server, 1).await;
    assert_eq!(
        quota.execute_bucket_warmup_at(now).await.unwrap().warmed_up,
        1
    );
}

#[tokio::test]
async fn final_usage_refresh_skips_external_usage_in_new_cycle() {
    let (store, server, now, reset) = setup().await;
    let quota = service(&store, &server);
    quota
        .execute_bucket_warmup_at(Utc.timestamp_opt(reset - 1, 0).unwrap())
        .await
        .unwrap();
    mount_usage(&server, reset + 604800, 1, 1).await;
    mount_warmup(&server, 0).await;
    assert_eq!(
        quota.execute_bucket_warmup_at(now).await.unwrap().warmed_up,
        0
    );
}

#[tokio::test]
async fn cron_warmup_counts_as_usage_without_percentage_change() {
    let (store, server, now, reset) = setup().await;
    let quota = service(&store, &server);
    quota
        .execute_bucket_warmup_at(Utc.timestamp_opt(reset - 1, 0).unwrap())
        .await
        .unwrap();
    mount_warmup(&server, 1).await;
    assert_eq!(
        quota
            .execute_warmup("another-model")
            .await
            .unwrap()
            .warmed_up,
        1
    );
    assert_eq!(
        quota.execute_bucket_warmup_at(now).await.unwrap().warmed_up,
        0
    );
}

#[tokio::test]
async fn pre_reset_usage_and_arbitrary_general_window_still_warm_up() {
    let (store, server, now, reset) = setup().await;
    store.set_quota_settings(
        "acct_bucket_warmup",
        serde_json::from_value(json!({
            "codex:86400s": {"warmup": {"model": "test-model"}}
        }))
        .unwrap(),
    );
    let account = store.account("acct_bucket_warmup").unwrap();
    let mut document = usage(reset, 0);
    document["rate_limit"]["secondary_window"]["limit_window_seconds"] = json!(86400);
    document["_cpr_bucket_last_used_at"] = json!({"codex": reset - 1});
    let observed_at = SystemTime::from(Utc.timestamp_opt(reset - 1, 0).unwrap());
    store
        .compare_and_swap_quota(QuotaObservation {
            quota_pause: None,
            expected_connection: None,
            plan_type: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: OpaqueProviderData::new(document.as_object().unwrap().clone()),
            observed_at,
            state: QuotaState::allowed(observed_at),
        })
        .await
        .unwrap();
    let quota = service(&store, &server);
    quota
        .execute_bucket_warmup_at(Utc.timestamp_opt(reset - 1, 0).unwrap())
        .await
        .unwrap();
    document["rate_limit"]["secondary_window"]["reset_at"] = json!(reset + 86400);
    document
        .as_object_mut()
        .unwrap()
        .remove("_cpr_bucket_last_used_at");
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(document))
        .expect(1)
        .mount(&server)
        .await;
    mount_warmup(&server, 1).await;
    assert_eq!(
        quota.execute_bucket_warmup_at(now).await.unwrap().warmed_up,
        1
    );
}
