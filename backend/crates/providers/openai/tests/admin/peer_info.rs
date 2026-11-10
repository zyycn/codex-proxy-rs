//! OpenAI 凭据适配、公共互联服务、Host 传输与调度只读端口的集成验证

use super::*;
use crate::support::MemoryCooldownPort;
use gateway_core::{
    account::{UpstreamConnection, UpstreamInfoSource},
    task::WorkerCycleContext,
};
use provider_openai::credential::ResponsesTransport;

fn peer_document() -> Value {
    let now = Utc::now();
    json!({
        "object":"cpr.peer", "schema_version":3,
        "observed_at":now.to_rfc3339(), "expires_at":(now + chrono::Duration::minutes(5)).to_rfc3339(),
        "capabilities":["key_usage", "pool_summary"],
        "usage":{"status":"available","observed_at":now.to_rfc3339(),"data":{"unit":"USD","allowed":true,"windows":[
            {"key":"daily","label":"日额度","window_seconds":86400,"limit_reached":false,"total":"10","used":"2","remaining":"8","reset_at":(now + chrono::Duration::days(1)).to_rfc3339()},
            {"key":"weekly","label":"周额度","window_seconds":604800,"limit_reached":false,"total":"50","used":"5","remaining":"45","reset_at":(now + chrono::Duration::days(7)).to_rfc3339()}
        ]}},
        "pool":{"account_count":2,"states":{"ready":2,"cooldown":0,"exhausted":0,"disabled":0,"error":0,"unknown":0},"plans":[{"name":"plus","count":2}]},
        "models":[]
    })
}

async fn exhaust_account(accounts: &MemoryAccountStore, id: &ProviderAccountId, at: SystemTime) {
    let account = accounts.account(id.as_str()).unwrap();
    accounts
        .apply_quota_access(QuotaAccessChange {
            account_id: id.clone(),
            expected_revision: account.revision(),
            state: QuotaState::exhausted(QuotaEvidence::UsageLimitReached, at, None),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn peer_sync_restores_exhausted_accounts_only_from_current_key_budget_facts() {
    for case in [
        "available",
        "unlimited",
        "budget_exhausted",
        "weekly_exhausted",
        "expired_budget",
        "missing_remaining",
        "missing_access",
        "missing_allowed",
        "undisclosed_access",
        "amounts_disagree",
        "usage_unknown",
        "budget_only",
        "access_only",
        "access_only_denied",
        "missing_usage",
        "empty_windows",
        "custom_window_exhausted",
        "old_usage",
        "old_snapshot",
        "unauthorized",
        "unavailable",
    ] {
        let server = MockServer::start().await;
        let (mut bundle, accounts, _, id, _config) = fixture(&server).await;
        let exhausted_at = SystemTime::now() - Duration::from_secs(60);
        exhaust_account(&accounts, &id, exhausted_at).await;
        let mut document = peer_document();
        let older = DateTime::<Utc>::from(exhausted_at - Duration::from_secs(1)).to_rfc3339();
        match case {
            "unlimited" => {
                for window in [0, 1] {
                    document["usage"]["data"]["windows"][window] = json!({
                        "key": (["daily", "weekly"][window]),
                        "total":null, "remaining":null, "used":"7", "reset_at":null
                    });
                }
            }
            "budget_exhausted" => {
                document["usage"]["data"]["allowed"] = json!(false);
                document["usage"]["data"]["windows"][0]["limit_reached"] = json!(true);
            }
            "weekly_exhausted" => {
                document["usage"]["data"]
                    .as_object_mut()
                    .unwrap()
                    .remove("allowed");
                document["usage"]["data"]["windows"][1]["limit_reached"] = json!(true);
            }
            "expired_budget" => {
                document["usage"]["data"]
                    .as_object_mut()
                    .unwrap()
                    .remove("allowed");
                document["usage"]["data"]["windows"][0]["limit_reached"] = json!(true);
                document["usage"]["data"]["windows"][0]["reset_at"] = json!(older);
            }
            "missing_allowed" => {
                document["usage"]["data"]
                    .as_object_mut()
                    .unwrap()
                    .remove("allowed");
            }
            "undisclosed_access" => document["capabilities"] = json!(["pool_summary"]),
            "missing_access" => {
                document["usage"]["data"]
                    .as_object_mut()
                    .unwrap()
                    .remove("allowed");
                for window in [0, 1] {
                    document["usage"]["data"]["windows"][window]
                        .as_object_mut()
                        .unwrap()
                        .remove("limit_reached");
                }
            }
            "amounts_disagree" => {
                document["usage"]["data"]["windows"][0]["remaining"] = json!("0");
                document["usage"]["data"]["windows"][0]["used"] = json!("10");
            }
            "missing_remaining" => {
                document["usage"]["data"]["windows"][0]["remaining"] = Value::Null
            }
            "usage_unknown" => document["usage"]["status"] = json!("unknown"),
            "budget_only" => document["capabilities"] = json!(["key_usage"]),
            "access_only" | "access_only_denied" => {
                document["usage"]["data"] = json!({"allowed": case == "access_only"});
                document.as_object_mut().unwrap().remove("pool");
            }
            "missing_usage" => {
                document.as_object_mut().unwrap().remove("usage");
            }
            "empty_windows" => document["usage"]["data"] = json!({"windows": []}),
            "custom_window_exhausted" => {
                document["usage"]["data"] = json!({
                    "unit":"USD", "windows":[{
                        "key":"monthly", "used":"100", "total":"100", "remaining":"0",
                        "limit_reached":true, "reset_at":(Utc::now() + chrono::Duration::days(3)).to_rfc3339()
                    }]
                });
            }
            "old_usage" => document["usage"]["observed_at"] = json!(older),
            "old_snapshot" => document["observed_at"] = json!(older),
            _ => {}
        }
        let status = match case {
            "unauthorized" => 403,
            "unavailable" => 503,
            _ => 200,
        };
        Mock::given(method("GET"))
            .and(path("/custom/v1/cpr/link"))
            .respond_with(ResponseTemplate::new(status).set_body_json(document))
            .expect(1)
            .mount(&server)
            .await;
        cycle(&worker(&mut bundle)).await;
        let account = accounts.account(id.as_str()).unwrap();
        assert_eq!(
            account.quota().is_exhausted(),
            !matches!(
                case,
                "available"
                    | "unlimited"
                    | "budget_only"
                    | "access_only"
                    | "missing_remaining"
                    | "amounts_disagree"
            ),
            "{case}"
        );
        assert_eq!(account.credential_state(), CredentialState::Ready);
        if matches!(
            case,
            "access_only" | "access_only_denied" | "empty_windows" | "missing_usage"
        ) {
            assert!(quota(&bundle, &id).await.windows.is_empty(), "{case}");
        }
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|request| request.method == "GET")
        );
    }
}

#[tokio::test]
async fn peer_sync_cannot_clear_an_exhaustion_recorded_while_the_request_is_in_flight() {
    let server = MockServer::start().await;
    let (mut bundle, accounts, _, id, _config) = fixture(&server).await;
    exhaust_account(&accounts, &id, SystemTime::now() - Duration::from_secs(60)).await;
    let mut document = peer_document();
    // 上游时钟略快时也必须按本地请求开始时间保护并发失败
    let future = (Utc::now() + chrono::Duration::seconds(10)).to_rfc3339();
    document["observed_at"] = json!(future);
    document["usage"]["observed_at"] = json!(future);
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(document)
                .set_delay(Duration::from_millis(200)),
        )
        .mount(&server)
        .await;
    let worker = Arc::new(worker(&mut bundle));
    let running = tokio::spawn({
        let worker = worker.clone();
        async move { cycle(&worker).await }
    });
    wait_for_request(&server).await;
    let failed_at = SystemTime::now();
    exhaust_account(&accounts, &id, failed_at).await;
    running.await.unwrap();
    let account = accounts.account(id.as_str()).unwrap();
    assert_eq!(
        account.quota(),
        QuotaState::exhausted(QuotaEvidence::UsageLimitReached, failed_at, None)
    );
}

#[tokio::test]
async fn key_budget_rejections_recover_through_sync_while_ordinary_429_keeps_its_cooldown() {
    use gateway_core::error::ProviderErrorKind;

    for code in [
        "key_daily_budget_exceeded",
        "key_weekly_budget_exceeded",
        "rate_limit_exceeded",
    ] {
        let budget = code != "rate_limit_exceeded";
        let server = MockServer::start().await;
        let (mut bundle, accounts, _, id, _config) = fixture(&server).await;
        Mock::given(method("POST"))
            .and(path("/custom/v1/responses"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "86400")
                .set_body_json(json!({"error":{"code":code,"type":if budget { "usage_limit_reached" } else { "rate_limit_error" },"message":"limit reached"}})))
            .expect(1).mount(&server).await;
        let mut response = execute_peer_request(&bundle, "round_robin").await.unwrap();
        let failure = loop {
            match response.next().await {
                Some(Err(error)) => break error,
                Some(Ok(_)) => {}
                None => panic!("expected rejection"),
            }
        };
        drop(response);
        assert_eq!(
            failure.kind(),
            if budget {
                ProviderErrorKind::QuotaExhausted
            } else {
                ProviderErrorKind::RateLimited
            },
            "{code}"
        );
        assert_eq!(
            accounts
                .account(id.as_str())
                .unwrap()
                .quota()
                .is_exhausted(),
            budget,
            "{code}"
        );
        let cooldown = bundle.cooldowns.read(&id).await.unwrap();
        assert_eq!(cooldown.is_some(), !budget, "{code}");
        server.verify().await;
        server.reset().await;

        Mock::given(method("GET"))
            .and(path("/custom/v1/cpr/link"))
            .respond_with(ResponseTemplate::new(200).set_body_json(peer_document()))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/custom/v1/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(COMPLETED_SESSION_SSE),
            )
            .expect(if budget { 1 } else { 0 })
            .mount(&server)
            .await;
        cycle(&worker(&mut bundle)).await;
        assert!(
            !accounts
                .account(id.as_str())
                .unwrap()
                .quota()
                .is_exhausted(),
            "{code}"
        );
        assert_eq!(
            bundle.cooldowns.read(&id).await.unwrap(),
            cooldown,
            "{code}"
        );
        if budget {
            let mut response = execute_peer_request(&bundle, "round_robin").await.unwrap();
            while let Some(event) = response.next().await {
                event.unwrap();
            }
        } else {
            let error = match execute_peer_request(&bundle, "round_robin").await {
                Err(error) => error,
                Ok(mut response) => loop {
                    match response.next().await {
                        Some(Err(error)) => break error,
                        Some(Ok(_)) => {}
                        None => panic!("cooldown must prevent sending"),
                    }
                },
            };
            assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
        }
        server.verify().await;
    }
}

async fn fixture(
    server: &MockServer,
) -> (
    PeerFixture,
    Arc<MemoryAccountStore>,
    Arc<MemoryAccountStore>,
    ProviderAccountId,
    TestOpenAiConfig,
) {
    let accounts = Arc::new(MemoryAccountStore::default());
    accounts
        .seed_api_key(
            "acct_peer",
            format!("{}/custom/v1", server.uri()),
            ResponsesTransport::Http,
        )
        .await;
    let id = ProviderAccountId::new("acct_peer").unwrap();
    let upstream = accounts.clone();
    let config = valid_config();
    let bundle = peer_fixture(config.config.clone(), accounts.clone()).await;
    (bundle, accounts, upstream, id, config)
}

fn worker(bundle: &mut PeerFixture) -> gateway_core::task::WorkerRegistration {
    bundle
        .take_worker_contributions()
        .into_iter()
        .find_map(|contribution| match contribution {
            WorkerContribution::Registration(registration)
                if registration.id.owner() == "peer-info" =>
            {
                Some(registration)
            }
            _ => None,
        })
        .unwrap()
}

async fn cycle(worker: &gateway_core::task::WorkerRegistration) {
    let WorkerRunnable::Scheduled { task, .. } = &worker.runnable else {
        panic!("scheduled")
    };
    task.run_cycle(WorkerCycleContext::new(
        worker.id.clone(),
        None,
        CancellationToken::new(),
    ))
    .await
    .unwrap();
}

async fn quota(
    bundle: &PeerFixture,
    id: &ProviderAccountId,
) -> gateway_admin::model::provider_credentials::ProviderQuota {
    match bundle.peer.service().read_quota(id).await.unwrap() {
        Some(quota) => quota,
        None => bundle
            .provider
            .admin_provider()
            .quota(ProviderQuotaRequest {
                account_id: id.clone(),
                refresh: false,
                rolling_usage: None,
            })
            .await
            .unwrap(),
    }
}

#[tokio::test]
async fn key_usage_retains_unlimited_windows_without_pool_disclosure() {
    for weekly_total in [None, Some("50")] {
        let server = MockServer::start().await;
        let mut document = peer_document();
        document["capabilities"] = json!(["key_usage"]);
        document.as_object_mut().unwrap().remove("pool");
        document["usage"]["data"]["windows"][0] =
            json!({"key":"daily","total":null,"used":"0","remaining":null,"reset_at":null});
        document["usage"]["data"]["windows"][1]["total"] = json!(weekly_total);
        document["usage"]["data"]["windows"][1]["remaining"] = json!(weekly_total.map(|_| "45"));
        Mock::given(method("GET"))
            .and(path("/custom/v1/cpr/link"))
            .respond_with(ResponseTemplate::new(200).set_body_json(document))
            .expect(1)
            .mount(&server)
            .await;
        let (mut bundle, _, _, id, _config) = fixture(&server).await;
        cycle(&worker(&mut bundle)).await;
        let observed = quota(&bundle, &id).await;
        assert!(observed.upstream.unwrap().pool.is_none());
        assert_eq!(observed.windows.len(), 2);
        let daily = &observed.windows[0];
        assert_eq!(daily.key, "peer:budget:daily");
        assert_eq!(daily.used_percent, None);
        assert!(!daily.limit_reached);
        let amount = daily.upstream.as_ref().unwrap().amount.as_ref().unwrap();
        assert_eq!(amount.used, "0");
        assert!(amount.total.is_none());
        assert!(amount.remaining.is_none());
        let weekly = &observed.windows[1];
        assert_eq!(weekly.key, "peer:budget:weekly");
        assert_eq!(weekly.used_percent, weekly_total.map(|_| 10.0));
        let amount = weekly.upstream.as_ref().unwrap().amount.as_ref().unwrap();
        assert_eq!(amount.used, "5");
        assert_eq!(amount.total.as_deref(), weekly_total);
        let summary = bundle
            .peer
            .service()
            .read_summary(&id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summary.windows, observed.windows);
    }
}

#[tokio::test]
async fn custom_budget_windows_preserve_their_period_without_inventing_other_windows() {
    let server = MockServer::start().await;
    let mut document = peer_document();
    document["capabilities"] = json!(["key_usage"]);
    document.as_object_mut().unwrap().remove("pool");
    let reset = Utc::now() + chrono::Duration::days(20);
    document["usage"]["data"] = json!({
        "unit":"USD", "allowed":true, "windows":[{
            "key":"monthly", "window_seconds":2592000,
            "total":"100", "used":"20", "remaining":"80", "reset_at":reset.to_rfc3339()
        }]
    });
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(document))
        .expect(1)
        .mount(&server)
        .await;
    let (mut bundle, _, _, id, _) = fixture(&server).await;
    cycle(&worker(&mut bundle)).await;
    let observed = quota(&bundle, &id).await;
    assert_eq!(observed.windows.len(), 1);
    let window = &observed.windows[0];
    assert_eq!(window.key, "peer:budget:monthly");
    assert_eq!(window.label, "monthly");
    assert_eq!(window.window_seconds, Some(2592000));
    assert_eq!(window.reset_at, Some(reset));
    assert_eq!(window.used_percent, Some(20.0));
    assert!(!window.upstream.as_ref().unwrap().stale);
}

#[tokio::test]
async fn model_only_snapshots_publish_signals_without_budget_or_account_disclosure() {
    for (coverage, state, exhausted) in [
        ("complete", "exhausted", Some(true)),
        ("complete", "available", Some(false)),
        ("partial", "exhausted", None),
    ] {
        let server = MockServer::start().await;
        let now = Utc::now();
        let document = json!({
            "object":"cpr.peer", "schema_version":3,
            "observed_at":now.to_rfc3339(), "expires_at":(now + chrono::Duration::minutes(5)).to_rfc3339(),
            "capabilities":["model_availability"],
            "models":[{"model":"gpt-5.4", "coverage":coverage, "state":state,
                "observed_at":now.to_rfc3339()}]
        });
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(document))
            .expect(1)
            .mount(&server)
            .await;
        let (mut bundle, accounts, _, id, _) = fixture(&server).await;
        let reader = gateway_core::account::UpstreamInfoReader::default();
        reader.bind(&bundle.peer.scheduling_source()).unwrap();
        cycle(&worker(&mut bundle)).await;
        let observed = quota(&bundle, &id).await;
        assert!(observed.upstream.unwrap().discovered);
        assert!(observed.windows.is_empty());
        assert_eq!(
            reader
                .signal(
                    &accounts.account(id.as_str()).unwrap(),
                    Some("gpt-5.4"),
                    SystemTime::now()
                )
                .map(|signal| signal.exhausted),
            exhausted,
            "{coverage} {state}"
        );
    }
}

#[tokio::test]
async fn manual_refresh_queries_upstream_and_merges_concurrent_refreshes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/custom/v1/cpr/link"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer sk-api-test-only",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(peer_document()))
        .expect(1)
        .mount(&server)
        .await;
    let (bundle, _, _, id, _config) = fixture(&server).await;
    let admin = bundle.admin_provider();
    admin.account_changed(&id).await.unwrap();
    assert!(server.received_requests().await.unwrap().is_empty());
    let (first, second) = tokio::join!(admin.refresh_quota(&id), admin.refresh_quota(&id));
    assert!(first.unwrap().upstream.unwrap().discovered);
    assert!(second.unwrap().upstream.unwrap().discovered);
    let observed = quota(&bundle, &id).await;
    assert!(observed.upstream.unwrap().discovered);
    assert_eq!(observed.windows[0].used_percent, Some(20.0));
    assert_eq!(
        observed.windows[0].local_usage_attribution,
        QuotaLocalUsageAttribution::AccountWide
    );
    assert!(observed.windows[0].local_usage.is_none());
    assert_eq!(
        observed.windows[0]
            .upstream
            .as_ref()
            .unwrap()
            .amount
            .as_ref()
            .unwrap()
            .remaining
            .as_deref(),
        Some("8")
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn native_windows_preserve_account_and_bucket_identity_without_pool_averaging() {
    let server = MockServer::start().await;
    let mut document = peer_document();
    document["capabilities"] = serde_json::json!(["quota_windows"]);
    let observed_at = document["observed_at"].clone();
    let expires_at = document["expires_at"].clone();
    let window = |bucket: &str, percent: f64| {
        serde_json::json!({
            "key": "weekly", "group": "model", "label": "周额度",
            "limit_id": bucket, "limit_name": bucket, "used_percent": percent,
            "window_seconds": 604800, "limit_reached": false
        })
    };
    let mut minimal_window = window("Codex", 90.0);
    minimal_window.as_object_mut().unwrap().remove("group");
    minimal_window.as_object_mut().unwrap().remove("label");
    document.as_object_mut().unwrap().remove("usage");
    document.as_object_mut().unwrap().remove("pool");
    document["quota"] = serde_json::json!({

        "accounts": [
            { "id": "first", "models": ["gpt-5.4"], "observed_at": observed_at,
              "expires_at": expires_at, "windows": [window("Codex", 10.0), window("Other", 30.0)] },
            { "id": "second", "models": ["gpt-5.4"], "observed_at": observed_at,
              "expires_at": expires_at, "windows": [minimal_window] }
        ]
    });
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(document))
        .mount(&server)
        .await;
    let (mut bundle, _, _, id, _) = fixture(&server).await;
    cycle(&worker(&mut bundle)).await;
    let observed = quota(&bundle, &id).await;
    assert_eq!(observed.windows.len(), 3);
    assert_eq!(observed.windows[2].group, "general");
    assert_eq!(observed.windows[2].label, "weekly");
    assert_eq!(
        observed
            .windows
            .iter()
            .map(|window| window
                .upstream
                .as_ref()
                .and_then(|upstream| upstream.account_id.as_deref()))
            .collect::<Vec<_>>(),
        vec![Some("first"), Some("first"), Some("second")]
    );
    assert_eq!(
        observed
            .windows
            .iter()
            .map(|window| window.used_percent)
            .collect::<Vec<_>>(),
        vec![Some(10.0), Some(30.0), Some(90.0)]
    );
    let scopes: std::collections::BTreeSet<_> = observed
        .windows
        .iter()
        .filter_map(|window| window.limit_id.as_deref())
        .collect();
    assert_eq!(scopes.len(), 3);
    assert!(
        observed
            .windows
            .iter()
            .all(|window| window.local_usage.is_none()
                && window.upstream.as_ref().unwrap().amount.is_none())
    );
}

#[tokio::test]
async fn transient_failure_preserves_observation_and_revocation_erases_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(peer_document()))
        .mount(&server)
        .await;
    let (mut bundle, _, _store, id, _config) = fixture(&server).await;
    let worker = worker(&mut bundle);
    cycle(&worker).await;
    let observed_at = quota(&bundle, &id).await.observed_at;
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    bundle.admin_provider().forget(&id);
    cycle(&worker).await;
    let failed = quota(&bundle, &id).await;
    assert_eq!(failed.observed_at, observed_at);
    assert_eq!(
        failed.upstream.unwrap().last_error.as_deref(),
        Some("unavailable")
    );
    assert_eq!(failed.windows.len(), 2);
    assert!(bundle.admin_provider().refresh_quota(&id).await.is_err());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    bundle.admin_provider().forget(&id);
    cycle(&worker).await;
    let revoked = quota(&bundle, &id).await;
    assert!(revoked.windows.is_empty());
    let info = revoked.upstream.unwrap();
    assert!(info.observed_at.is_none());
    assert!(info.discovered);

    // 同为 403，仅共享关闭的协议码能改变识别结果
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "error": { "code": "peer_sharing_disabled" }
        })))
        .mount(&server)
        .await;
    bundle.admin_provider().forget(&id);
    cycle(&worker).await;
    let disabled = quota(&bundle, &id).await;
    let info = disabled.upstream.unwrap();
    assert!(!info.discovered);
    assert_eq!(info.last_error.as_deref(), Some("sharing_disabled"));
    assert!(disabled.windows.is_empty());

    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(peer_document()))
        .mount(&server)
        .await;
    bundle.admin_provider().forget(&id);
    let recovered = bundle.admin_provider().refresh_quota(&id).await.unwrap();
    assert!(recovered.upstream.unwrap().discovered);
    assert!(!recovered.windows.is_empty());
}

async fn wait_for_request(server: &MockServer) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !server.received_requests().await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn disabling_during_an_in_flight_request_fences_the_late_observation() {
    for status in [200, 404] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(status)
                    .set_delay(Duration::from_millis(200))
                    .set_body_json(peer_document()),
            )
            .mount(&server)
            .await;
        let (mut bundle, accounts, store, id, _config) = fixture(&server).await;
        let worker = Arc::new(worker(&mut bundle));
        let running = tokio::spawn({
            let worker = worker.clone();
            async move {
                cycle(&worker).await;
            }
        });
        wait_for_request(&server).await;
        accounts.set_enabled(&id, false).await.unwrap();
        bundle.admin_provider().account_changed(&id).await.unwrap();
        running.await.unwrap();
        assert!(
            store
                .get_quotas(std::slice::from_ref(&id))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(quota(&bundle, &id).await.upstream.is_none());
    }
}

#[tokio::test]
async fn cancellation_releases_refresh_and_restart_can_retry() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(2))
                .set_body_json(peer_document()),
        )
        .mount(&server)
        .await;
    let (mut bundle, _, store, id, _config) = fixture(&server).await;
    let worker = worker(&mut bundle);
    let cancellation = CancellationToken::new();
    let context = WorkerCycleContext::new(worker.id.clone(), None, cancellation.clone());
    let running = tokio::spawn(async move {
        let WorkerRunnable::Scheduled { task, .. } = worker.runnable else {
            panic!("scheduled")
        };
        task.run_cycle(context).await.unwrap();
    });
    wait_for_request(&server).await;
    cancellation.cancel();
    tokio::time::timeout(Duration::from_millis(100), running)
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .get_quotas(std::slice::from_ref(&id))
            .await
            .unwrap()
            .is_empty()
    );
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(peer_document()))
        .mount(&server)
        .await;
    let restarted = peer_fixture(_config.config.clone(), store).await;
    assert!(
        restarted
            .admin_provider()
            .refresh_quota(&id)
            .await
            .unwrap()
            .upstream
            .unwrap()
            .discovered
    );
}

#[tokio::test]
async fn invalid_or_redirected_responses_never_replace_the_last_valid_snapshot() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(peer_document()))
        .mount(&server)
        .await;
    let (mut bundle, _, _store, id, _config) = fixture(&server).await;
    let worker = worker(&mut bundle);
    cycle(&worker).await;
    let observed_at = quota(&bundle, &id).await.observed_at;
    let mut wrong_schema = peer_document();
    wrong_schema["schema_version"] = json!(2);
    let mut wrong_object = peer_document();
    wrong_object["object"] = json!("other.peer");
    let mut negative = peer_document();
    negative["usage"]["data"]["windows"][0]["used"] = json!("-20");
    let mut missing_unit = peer_document();
    missing_unit["usage"]["data"]
        .as_object_mut()
        .unwrap()
        .remove("unit");
    let mut duplicate_windows = peer_document();
    duplicate_windows["usage"]["data"]["windows"][1]["key"] = json!("daily");
    let mut future = peer_document();
    future["observed_at"] = json!((Utc::now() + chrono::Duration::minutes(10)).to_rfc3339());
    let mut huge_pool = peer_document();
    huge_pool["pool"]["account_count"] = json!(100_000_000);
    huge_pool["pool"]["states"]["ready"] = json!(100_000_000);
    let mut conflicting_states = peer_document();
    conflicting_states["pool"]["states"]["ready"] = json!(3);
    let mut conflicting_plans = peer_document();
    conflicting_plans["pool"]["plans"] =
        json!([{"name":"Pro","count":2},{"name":"Plus","count":2}]);
    let responses = [
        ResponseTemplate::new(200).set_body_json(huge_pool),
        ResponseTemplate::new(200).set_body_json(conflicting_states),
        ResponseTemplate::new(200).set_body_json(conflicting_plans),
        ResponseTemplate::new(200).set_body_json(wrong_schema),
        ResponseTemplate::new(200).set_body_json(wrong_object),
        ResponseTemplate::new(200).set_body_json(negative),
        ResponseTemplate::new(200).set_body_json(missing_unit),
        ResponseTemplate::new(200).set_body_json(duplicate_windows),
        ResponseTemplate::new(200).set_body_json(future),
        ResponseTemplate::new(200).set_body_raw(
            "x".repeat(gateway_protocol::peer::MAX_RESPONSE_BYTES + 1),
            "application/json",
        ),
        ResponseTemplate::new(302)
            .insert_header("location", format!("{}/never-follow", server.uri())),
    ];
    for response in responses {
        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/custom/v1/cpr/link"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/never-follow"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        bundle.admin_provider().forget(&id);
        cycle(&worker).await;
        let quota = quota(&bundle, &id).await;
        assert_eq!(quota.observed_at, observed_at);
        assert_eq!(quota.windows.len(), 2);
        assert!(quota.upstream.unwrap().last_error.is_some());
        server.verify().await;
    }
}

#[tokio::test]
async fn peer_sync_excludes_budget_exhausted_accounts_without_pool_disclosure() {
    let server = MockServer::start().await;
    let mut peer = peer_document();
    peer["capabilities"] = json!(["key_usage"]);
    peer.as_object_mut().unwrap().remove("pool");
    peer["usage"]["data"]["allowed"] = json!(false);
    peer["usage"]["data"]["windows"][0]["limit_reached"] = json!(true);
    peer["usage"]["data"]["windows"][0]["used"] = json!("10.0000");
    peer["usage"]["data"]["windows"][0]["remaining"] = json!("0.0000");
    Mock::given(method("GET"))
        .and(path("/custom/v1/cpr/link"))
        .respond_with(ResponseTemplate::new(200).set_body_json(peer))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/available/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(COMPLETED_SESSION_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/custom/v1/responses"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let (mut bundle, accounts, _, _, _config) = fixture(&server).await;
    accounts
        .seed_api_key(
            "acct_available",
            format!("{}/available/v1", server.uri()),
            ResponsesTransport::Http,
        )
        .await;
    cycle(&worker(&mut bundle)).await;
    assert!(
        accounts
            .account("acct_peer")
            .unwrap()
            .quota()
            .is_exhausted()
    );
    let mut response = execute_peer_request(&bundle, "smart").await.unwrap();
    assert_eq!(
        response.metadata().provider_account_id().as_str(),
        "acct_available"
    );
    while let Some(event) = response.next().await {
        event.unwrap();
    }
    server.verify().await;
}

#[tokio::test]
async fn peer_key_limits_set_default_capacity_and_lease_without_model_or_pool_disclosure() {
    use gateway_core::account::{AccountConcurrencyLimit, AccountWeight};
    for (limits, observed, stale, local_limit, expected) in [
        (
            Some(json!({"max_concurrency":5})),
            (Some(5), None),
            false,
            None,
            5,
        ),
        (
            Some(json!({"max_concurrency":5,"requests_per_minute":60})),
            (Some(5), Some(60)),
            false,
            Some(1),
            1,
        ),
        (
            Some(json!({"max_concurrency":0})),
            (Some(0), None),
            false,
            None,
            2,
        ),
        (
            Some(json!({"requests_per_minute":60})),
            (None, Some(60)),
            false,
            None,
            2,
        ),
        (None, (None, None), false, None, 2),
        (
            Some(json!({"max_concurrency":5})),
            (Some(5), None),
            true,
            None,
            2,
        ),
    ] {
        let server = MockServer::start().await;
        let now = Utc::now();
        let observed_at = if stale {
            now - chrono::Duration::minutes(6)
        } else {
            now
        };
        let mut document = json!({
            "object":"cpr.peer", "schema_version":3,
            "capabilities": ["key_limits"],
            "observed_at":observed_at.to_rfc3339(), "expires_at":(now + chrono::Duration::minutes(2)).to_rfc3339()
        });
        if let Some(limits) = limits {
            document["key_limits"] = limits;
        }
        Mock::given(method("GET"))
            .and(path("/custom/v1/cpr/link"))
            .respond_with(ResponseTemplate::new(200).set_body_json(document))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/custom/v1/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(COMPLETED_SESSION_SSE),
            )
            .expect(1)
            .mount(&server)
            .await;
        let (mut bundle, accounts, _, id, _config) = fixture(&server).await;
        accounts.set_scheduling(
            id.as_str(),
            local_limit.and_then(AccountConcurrencyLimit::new),
            AccountWeight::DEFAULT,
        );
        cycle(&worker(&mut bundle)).await;
        let info = quota(&bundle, &id).await.upstream.unwrap();
        assert_eq!((info.max_concurrency, info.requests_per_minute), observed);
        let version = accounts
            .get_quota_versions(std::slice::from_ref(&id))
            .await
            .unwrap()
            .pop()
            .unwrap();
        let inherited = bundle
            .admin_provider()
            .observed_concurrency_limit(&version, SystemTime::now());
        assert_eq!(
            inherited.map(|limit| u64::from(limit.get())),
            observed.0.filter(|limit| !stale && *limit > 0)
        );
        let mut stream = execute_peer_request(&bundle, "round_robin").await.unwrap();
        let capacity = stream
            .metadata()
            .selection_observation()
            .unwrap()
            .capacity()
            .unwrap();
        assert_eq!(capacity.total_slots(), u64::from(expected));
        assert_eq!(
            bundle
                .leases
                .requests
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .max_concurrent()
                .get(),
            expected
        );
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        server.verify().await;
    }
}

#[tokio::test]
async fn lists_reuse_versioned_summaries_and_details_keep_node_windows() {
    let server = MockServer::start().await;
    let mut document = peer_document();
    document["capabilities"] = json!(["key_usage", "pool_summary", "quota_windows"]);
    document["quota"] = json!({"accounts":[{
        "id":"node-one", "models":["gpt-5.4"],
        "observed_at":document["observed_at"], "expires_at":document["expires_at"],
        "windows":[{"key":"weekly","group":"general","label":"周额度","used_percent":40,"limit_reached":false}]
    }]});
    Mock::given(method("GET"))
        .and(path("/custom/v1/cpr/link"))
        .respond_with(ResponseTemplate::new(200).set_body_json(document))
        .expect(1)
        .mount(&server)
        .await;
    let (mut bundle, accounts, store, id, config) = fixture(&server).await;
    let worker = worker(&mut bundle);
    cycle(&worker).await;
    let account = account_record(&accounts.account(id.as_str()).unwrap());
    let requests = [ProviderQuotaReadRequest {
        account: &account,
        rolling_usage: None,
    }];
    let read_count = store.quota_reads();
    for _ in 0..3 {
        let summary = bundle
            .admin_provider()
            .read_quotas(&requests)
            .await
            .pop()
            .unwrap()
            .unwrap();
        assert_eq!(summary.windows.len(), 2);
        assert!(
            summary.windows.iter().all(|window| window
                .upstream
                .as_ref()
                .unwrap()
                .account_id
                .is_none())
        );
        assert!(summary.upstream.as_ref().unwrap().discovered);
        assert_eq!(summary.upstream.unwrap().pool.unwrap().account_count, 2);
        let single = bundle
            .admin_provider()
            .read_summary(&id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(single.windows.len(), 2);
        assert!(single.upstream.as_ref().unwrap().discovered);
        cycle(&worker).await;
    }
    assert_eq!(store.quota_reads(), read_count);
    let detail = quota(&bundle, &id).await;
    assert_eq!(detail.windows.len(), 3);
    assert_eq!(
        detail.windows[2]
            .upstream
            .as_ref()
            .unwrap()
            .account_id
            .as_deref(),
        Some("node-one")
    );

    let restored = peer_fixture(config.config.clone(), accounts).await;
    let summary = restored
        .admin_provider()
        .read_quotas(&requests)
        .await
        .pop()
        .unwrap()
        .unwrap();
    assert_eq!(summary.windows.len(), 2);
    let mut changed = store
        .get_quotas(std::slice::from_ref(&id))
        .await
        .unwrap()
        .remove(0);
    let mut document = changed.quota.expose_to_provider().clone();
    document.get_mut("pool").unwrap()["plans"][0]["name"] = json!("Team");
    changed.quota = OpaqueProviderData::new(document);
    changed.observed_at = SystemTime::now();
    store.compare_and_swap_quota(changed).await.unwrap();
    let read_count = store.quota_reads();
    let summary = restored
        .admin_provider()
        .read_quotas(&requests)
        .await
        .pop()
        .unwrap()
        .unwrap();
    assert_eq!(
        summary.upstream.unwrap().pool.unwrap().plans[0].name,
        "Team"
    );
    assert_eq!(store.quota_reads(), read_count + 1);
    let mut cleared = store
        .get_quotas(std::slice::from_ref(&id))
        .await
        .unwrap()
        .remove(0);
    cleared.quota = OpaqueProviderData::new(Default::default());
    cleared.observed_at = SystemTime::now();
    store.compare_and_swap_quota(cleared).await.unwrap();
    let read_count = store.quota_reads();
    let summary = restored
        .admin_provider()
        .read_quotas(&requests)
        .await
        .pop()
        .unwrap()
        .unwrap();
    assert!(summary.windows.is_empty());
    let info = summary.upstream.unwrap();
    assert!(!info.discovered);
    assert!(info.observed_at.is_none());
    assert_eq!(store.quota_reads(), read_count + 1);
}

#[tokio::test]
async fn cached_observations_require_the_current_protocol_version() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(peer_document()))
        .expect(1)
        .mount(&server)
        .await;
    let (mut bundle, accounts, store, id, config) = fixture(&server).await;
    cycle(&worker(&mut bundle)).await;
    let mut observation = store
        .get_quotas(std::slice::from_ref(&id))
        .await
        .unwrap()
        .remove(0);
    let mut document = observation.quota.expose_to_provider().clone();
    document.insert("schema_version".to_owned(), json!(99));
    observation.quota = OpaqueProviderData::new(document);
    observation.observed_at = SystemTime::now();
    store.compare_and_swap_quota(observation).await.unwrap();

    let restored = peer_fixture(config.config.clone(), accounts).await;
    let observed = quota(&restored, &id).await;
    assert!(!observed.upstream.unwrap().discovered);
    assert!(observed.windows.is_empty());
}

#[tokio::test]
async fn ordinary_api_upstreams_persist_detection_without_changing_quota_access() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/custom/v1/cpr/link"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    let (mut bundle, accounts, store, id, config) = fixture(&server).await;
    exhaust_account(&accounts, &id, SystemTime::now()).await;
    let worker = worker(&mut bundle);
    cycle(&worker).await;
    cycle(&worker).await;
    let info = quota(&bundle, &id).await.upstream.unwrap();
    assert!(!info.discovered);
    assert!(info.observed_at.is_none());
    assert_eq!(info.last_error.as_deref(), Some("unsupported"));
    assert!(bundle.admin_provider().refresh_quota(&id).await.is_ok());
    assert_eq!(
        store
            .get_account(&id)
            .await
            .unwrap()
            .unwrap()
            .quota()
            .access(),
        gateway_core::account::QuotaAccessState::Exhausted
    );
    let mut restored = peer_fixture(config.config.clone(), accounts).await;
    let info = quota(&restored, &id).await.upstream.unwrap();
    assert!(!info.discovered);
    assert_eq!(info.last_error.as_deref(), Some("unsupported"));
    cycle(&self::worker(&mut restored)).await;
}

#[tokio::test]
async fn cold_lists_bound_full_snapshot_batches_and_reuse_every_hydrated_summary() {
    let server = MockServer::start().await;
    let (bundle, accounts, store, first_id, _config) = fixture(&server).await;
    let mut records = Vec::new();
    for index in 0..20 {
        let id = if index == 0 {
            first_id.clone()
        } else {
            let id = format!("acct_peer_{index}");
            accounts
                .seed_api_key(
                    &id,
                    format!("{}/custom/v1", server.uri()),
                    ResponsesTransport::Http,
                )
                .await;
            ProviderAccountId::new(id).unwrap()
        };
        let account = accounts.account(id.as_str()).unwrap();
        let connection = UpstreamConnection::for_account(&account);
        let now = SystemTime::now();
        let mut document = peer_document();
        document["usage"]["data"]["windows"][0]["used"] = json!(index.to_string());
        store
            .compare_and_swap_quota(QuotaObservation {
                quota_pause: None,
                expected_connection: Some(connection),
                account_id: id.clone(),
                expected_revision: account.revision(),
                quota: OpaqueProviderData::new(document.as_object().unwrap().clone()),
                plan_type: None,
                observed_at: now,
                state: QuotaState::unknown(),
            })
            .await
            .unwrap();
        records.push(account_record(&account));
    }
    let requests = records
        .iter()
        .map(|account| ProviderQuotaReadRequest {
            account,
            rolling_usage: None,
        })
        .collect::<Vec<_>>();
    for _ in 0..3 {
        let quotas = bundle.admin_provider().read_quotas(&requests).await;
        assert_eq!(quotas.len(), records.len());
        for (index, quota) in quotas.into_iter().enumerate() {
            let quota = quota.unwrap();
            assert_eq!(quota.windows.len(), 2);
            assert_eq!(
                quota.windows[0]
                    .upstream
                    .as_ref()
                    .unwrap()
                    .amount
                    .as_ref()
                    .unwrap()
                    .used,
                index.to_string()
            );
        }
        assert_eq!(store.quota_reads(), records.len() / 2);
    }
    assert!(store.largest_quota_read() <= 2);

    for account in [&records[0], records.last().unwrap()] {
        let id = ProviderAccountId::new(account.id.clone()).unwrap();
        let mut observation = store.get_quotas(&[id]).await.unwrap().remove(0);
        observation.observed_at = SystemTime::now();
        store.compare_and_swap_quota(observation).await.unwrap();
    }
    let read_count = store.quota_reads();
    let quotas = bundle.admin_provider().read_quotas(&requests).await;
    assert!(quotas.into_iter().all(|quota| quota.is_ok()));
    assert_eq!(store.quota_reads(), read_count + 1);
}

async fn execute_peer_request(
    bundle: &PeerFixture,
    strategy: &str,
) -> Result<gateway_core::engine::provider::ProviderStream, gateway_core::error::ProviderError> {
    let provider = ProviderKind::new("openai").unwrap();
    let model = UpstreamModelId::new("gpt-5.4").unwrap();
    let scope = Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([
            (
                ProviderAccountId::new("acct_peer").unwrap(),
                RuntimeAccount::new(provider.clone(), BTreeSet::new()),
            ),
            (
                ProviderAccountId::new("acct_available").unwrap(),
                RuntimeAccount::new(provider.clone(), BTreeSet::new()),
            ),
        ]))),
        ClientRoutingScope::all_accounts(),
    ));
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            json!({"model":"gpt-5.4", "input":"budget routing"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap(),
    ));
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        gateway_core::settings::SettingsValues::new(2, strategy, Default::default(), None, None),
        vec![provider.clone()],
        vec![ProviderModel::new(
            provider,
            model,
            ModelCapabilities::new(BTreeSet::from([operation.kind()]), Some(32_000))
                .with_upstream_feature_validation(),
        )],
        Vec::new(),
    )
    .unwrap();
    let plan = snapshot
        .plan(
            &PublicModelId::new("gpt-5.4").unwrap(),
            &operation,
            scope.clone(),
            &RoutingContext::default(),
        )
        .unwrap();
    let context = AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_peer_smart").unwrap(),
            ClientApiKeyId::new("key_peer_smart").unwrap(),
        ),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(5),
        gateway_core::account::AccountSelectionPolicy::new(
            gateway_core::account::RotationStrategy::parse(strategy).unwrap(),
            NonZeroU32::new(2).unwrap(),
        ),
        AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(scope),
        None,
        CancellationToken::new(),
    );
    bundle
        .core_provider()
        .execute(
            ProviderRequest::new(operation, plan.candidates()[0].clone()),
            context,
        )
        .await
}

struct PeerFixture {
    leases: Arc<TestLeaseCoordinator>,
    provider: provider_openai::ProviderBundle,
    peer: gateway_admin::PeerUpstreamBundle,
    cooldowns: Arc<MemoryCooldownPort>,
}

impl PeerFixture {
    fn admin_provider(&self) -> Arc<dyn gateway_admin::ports::peer::PeerUpstreamService> {
        self.peer.service()
    }

    fn core_provider(&self) -> Arc<dyn gateway_core::engine::provider::Provider> {
        self.provider.core_provider()
    }

    fn take_worker_contributions(&mut self) -> Vec<WorkerContribution> {
        self.peer.take_worker_contributions()
    }
}

async fn peer_fixture(
    config: provider_openai::OpenAiConfig,
    accounts: Arc<MemoryAccountStore>,
) -> PeerFixture {
    let reader = Arc::new(gateway_core::account::UpstreamInfoReader::default());
    let cooldowns = Arc::new(MemoryCooldownPort::new());
    let leases = Arc::new(TestLeaseCoordinator::default());
    let provider = provider_openai::initialize(
        config,
        ProviderStorePorts::new(
            accounts.clone(),
            leases.clone(),
            Arc::new(MemorySessionAffinity::default()),
            Arc::new(MemorySessionExclusions::default()),
            Arc::new(TestCatalogCache::default()),
            Arc::new(TestArtifactProfiles),
            Arc::new(TestCredentialState),
            cooldowns.clone(),
            Arc::new(TestRuntimePolicy),
            Arc::new(TestOAuthPending::default()),
            Arc::new(RecordingDiagnostics::default()),
        ),
        reader.clone(),
    )
    .await
    .unwrap();
    let registry =
        gateway_admin::ports::provider::ProviderAdminRegistry::new([provider.admin_provider()])
            .unwrap();
    let peer = gateway_admin::initialize_peer_upstream(
        accounts.clone(),
        &registry,
        Arc::new(gateway_host::peer::HttpPeerTransport::with_client_builder(
            provider_openai::build_reqwest_client_with_custom_ca,
        )),
    )
    .unwrap();
    reader.bind(&peer.scheduling_source()).unwrap();
    PeerFixture {
        leases,
        provider,
        peer,
        cooldowns,
    }
}
