use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{TimeZone as _, Utc};
use futures::future::join_all;
use gateway_core::engine::credential::{
    AccountAvailability, AccountStateChange, OpaqueProviderData, ProviderAccountStore as _,
    QuotaObservation, QuotaWriteOutcome,
};
use provider_openai::OFFICIAL_CODEX_BASE_URL;
use provider_openai::credential::{
    CodexCredentialQuotaService, CodexQuotaSyncSummary, CodexQuotaWindowKind,
    ImportCodexOAuthCredential, parse_codex_quota_usage,
};
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{MemoryAccountStore, profile, secret};

#[test]
fn parser_extracts_dynamic_windows_without_a_fixed_database_shape() {
    let fact = parse_codex_quota_usage(&json!({
        "rate_limit": {
            "allowed": true,
            "primary_window": {"used_percent": 25.2, "reset_at": 1_800_000_100},
            "secondary_window": {"used_percent": 80.4, "reset_at": 1_800_000_200}
        },
        "additional_rate_limits": [{
            "limit_name": "future_dynamic_window",
            "rate_limit": {"primary_window": {"used_percent": 10}}
        }]
    }))
    .expect("valid dynamic quota");

    assert_eq!(fact.remaining_percent(), Some(20));
    assert_eq!(
        fact.resets_at().map(|value| value.timestamp()),
        Some(1_800_000_100)
    );
    assert!(!fact.exhausted());
}

#[test]
fn parser_treats_any_confirmed_provider_limit_as_exhausted() {
    let fact = parse_codex_quota_usage(&json!({
        "rate_limit": {"primary_window": {"used_percent": 10}},
        "additional_rate_limits": [{
            "rate_limit": {"allowed": false, "primary_window": {"used_percent": 100}}
        }]
    }))
    .expect("valid quota");

    assert!(fact.exhausted());
}

#[test]
fn parser_does_not_infer_exhaustion_from_unknown_credit_fields() {
    let fact = parse_codex_quota_usage(&json!({
        "credits": {
            "has_credits": false,
            "balance": 0,
            "overage_limit_reached": false,
            "future_provider_field": {"anything": true}
        }
    }))
    .expect("recognized credits object");

    assert!(!fact.exhausted());
}

#[test]
fn parser_accepts_official_null_additional_rate_limits() {
    let fact = parse_codex_quota_usage(&json!({
        "rate_limit": {
            "allowed": true,
            "limit_reached": false,
            "primary_window": {
                "limit_window_seconds": 18_000,
                "used_percent": 12.5,
                "reset_at": 1_800_000_100
            },
            "secondary_window": {
                "limit_window_seconds": 604_800,
                "used_percent": 30.0,
                "reset_at": 1_800_000_200
            }
        },
        "additional_rate_limits": null,
        "credits": {
            "has_credits": false,
            "balance": null,
            "overage_limit_reached": false,
            "unlimited": false
        },
        "spend_control": {
            "individual_limit": null,
            "reached": false
        }
    }))
    .expect("official null additional quota");

    assert_eq!(fact.remaining_percent(), Some(70));
    assert!(!fact.exhausted());
}

#[test]
fn parser_rejects_wrong_known_field_type_without_echoing_body() {
    let marker = "quota-secret-marker";
    let error = parse_codex_quota_usage(&json!({
        "rate_limit": {"allowed": marker}
    }))
    .expect_err("known field type must be strict");

    assert!(!format!("{error:?} {error}").contains(marker));
}

#[test]
fn parser_rejects_unrecognized_top_level_object() {
    assert!(parse_codex_quota_usage(&json!({"future_only": {"used": 1}})).is_err());
}

fn wire_profile() -> CodexWireProfileState {
    CodexWireProfileState::new(CodexWireProfile {
        originator: "codex_cli_rs".to_owned(),
        codex_version: "0.144.0".to_owned(),
        desktop_version: "1.0.0".to_owned(),
        desktop_build: "1".to_owned(),
        os_type: "linux".to_owned(),
        os_version: "6.8".to_owned(),
        arch: "x86_64".to_owned(),
        terminal: "quota-contract".to_owned(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 7, 18, 0, 0, 0)
            .single()
            .expect("fixture time"),
    })
}

fn quota_service(store: &Arc<MemoryAccountStore>) -> CodexCredentialQuotaService {
    quota_service_with_http(
        store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
    )
}

fn quota_service_with_http(
    store: &Arc<MemoryAccountStore>,
    http: reqwest::Client,
) -> CodexCredentialQuotaService {
    quota_service_with_base_url(store, http, OFFICIAL_CODEX_BASE_URL.to_owned())
}

fn quota_service_with_base_url(
    store: &Arc<MemoryAccountStore>,
    http: reqwest::Client,
    base_url: String,
) -> CodexCredentialQuotaService {
    CodexCredentialQuotaService::new(
        store.repository(),
        wire_profile(),
        http,
        base_url,
        crate::support::agent_identity_service(store),
    )
}

fn blocked_network_quota_service(store: &Arc<MemoryAccountStore>) -> CodexCredentialQuotaService {
    let proxy = reqwest::Proxy::all("http://127.0.0.1:9").expect("loopback proxy");
    let http = reqwest::Client::builder()
        .proxy(proxy)
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(1))
        .build()
        .expect("blocked client");
    quota_service_with_http(store, http)
}

async fn create_account(store: &Arc<MemoryAccountStore>, account_id: &str) {
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: account_id.to_owned(),
            name: account_id.to_owned(),
            secret: secret(&format!("token-{account_id}")),
            verified_account: profile(&format!("chatgpt-{account_id}")),
            next_refresh_at: Some(Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
}

#[tokio::test]
async fn concurrent_cold_scheduling_hydration_reads_quota_once() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_hydration").await;
    let account = store.account("acct_hydration").expect("created account");
    let service = quota_service(&store);

    join_all((0..32).map(|_| service.prepare_scheduling(std::slice::from_ref(&account)))).await;

    assert_eq!(store.quota_reads(), 1);
}

#[tokio::test]
async fn persisted_provider_quota_projects_dynamic_windows_without_network_io() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_quota").await;
    let account = store.account("acct_quota").expect("created account");
    let raw = json!({
        "rate_limit": {
            "allowed": true,
            "primary_window": {
                "used_percent": 37,
                "reset_at": 1_900_000_000,
                "limit_window_seconds": 18_000
            },
            "secondary_window": {
                "used_percent": 42,
                "reset_at": 1_900_604_800,
                "limit_window_seconds": 604_800
            }
        },
        "additional_rate_limits": null,
        "spend_control": {
            "reached": false,
            "individual_limit": {
                "used_percent": 12,
                "reset_at": 1_902_592_000
            }
        },
        "provider_specific_root": {"opaque": [1, 2, 3]}
    });
    let outcome = store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: Some(OpaqueProviderData::new(
                raw.as_object().expect("quota object").clone(),
            )),
            observed_at: Some(SystemTime::now()),
        })
        .await
        .expect("persist quota");
    assert_eq!(outcome, QuotaWriteOutcome::Updated);

    let snapshot = quota_service(&store)
        .read_account(account.id())
        .await
        .expect("read quota")
        .expect("quota snapshot");

    assert_eq!(snapshot.windows().len(), 3);
    assert_eq!(
        snapshot.windows()[0].kind(),
        CodexQuotaWindowKind::ShortTerm
    );
    assert_eq!(snapshot.windows()[0].window_seconds(), Some(18_000));
    assert_eq!(snapshot.windows()[1].kind(), CodexQuotaWindowKind::Weekly);
    assert_eq!(snapshot.windows()[2].kind(), CodexQuotaWindowKind::Monthly);
}

#[tokio::test]
async fn persisted_codex_additional_limit_replaces_the_top_level_rate_limit() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_canonical_codex_limit").await;
    let account = store
        .account("acct_canonical_codex_limit")
        .expect("created account");
    let raw = json!({
        "rate_limit": {
            "primary_window": {
                "used_percent": 91,
                "reset_at": 1_900_000_000,
                "limit_window_seconds": 2_592_000
            }
        },
        "additional_rate_limits": [{
            "metered_feature": "codex",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 2,
                    "reset_at": 1_900_000_000,
                    "limit_window_seconds": 2_592_000
                }
            }
        }]
    });
    let outcome = store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: Some(OpaqueProviderData::new(
                raw.as_object().expect("quota object").clone(),
            )),
            observed_at: Some(SystemTime::now()),
        })
        .await
        .expect("persist quota");
    assert_eq!(outcome, QuotaWriteOutcome::Updated);

    let snapshot = quota_service(&store)
        .read_account(account.id())
        .await
        .expect("read quota")
        .expect("quota snapshot");
    let monthly = snapshot
        .windows()
        .iter()
        .filter(|window| window.kind() == CodexQuotaWindowKind::Monthly)
        .collect::<Vec<_>>();

    assert_eq!(snapshot.fact().remaining_percent(), Some(98));
    assert_eq!(monthly.len(), 1);
    assert_eq!(monthly[0].source(), "core");
    assert_eq!(monthly[0].used_percent(), Some(2.0));
}

#[tokio::test]
async fn passive_rate_limit_headers_update_quota_and_account_state_with_revision_fence() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_passive_quota").await;
    let account = store.account("acct_passive_quota").expect("account");
    let service = quota_service(&store);
    let reset_at = 1_900_000_000_i64;
    let headers = vec![
        ("x-codex-active-limit".to_owned(), "codex".to_owned()),
        ("x-codex-primary-used-percent".to_owned(), "100".to_owned()),
        (
            "x-codex-primary-window-minutes".to_owned(),
            "300".to_owned(),
        ),
        ("x-codex-primary-reset-at".to_owned(), reset_at.to_string()),
        ("x-codex-limit-reached".to_owned(), "true".to_owned()),
    ];

    assert!(
        service
            .synchronize_passive_headers(&account, &headers)
            .await
            .expect("passive quota")
    );
    let current = store
        .account("acct_passive_quota")
        .expect("current account");
    assert_eq!(current.availability(), AccountAvailability::QuotaExhausted);
    let snapshot = service
        .read_account(account.id())
        .await
        .expect("read quota")
        .expect("quota snapshot");
    assert_eq!(snapshot.windows()[0].window_seconds(), Some(18_000));
    assert_eq!(
        snapshot.windows()[0]
            .reset_at()
            .map(|value| value.timestamp()),
        Some(reset_at)
    );
}

#[tokio::test]
async fn manual_quota_refresh_updates_account_state_from_provider_response() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_manual_quota_state";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("created account");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header(
            "authorization",
            format!("Bearer token-{account_id}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": false,
                "limit_reached": true,
                "primary_window": {"used_percent": 100, "reset_at": 1_900_000_000}
            }
        })))
        .mount(&server)
        .await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
    );

    let exhausted = service
        .refresh_account(account.id())
        .await
        .expect("refresh exhausted quota");

    assert!(exhausted.fact().exhausted());
    assert_eq!(
        store
            .account(account_id)
            .expect("exhausted account")
            .availability(),
        AccountAvailability::QuotaExhausted
    );

    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header(
            "authorization",
            format!("Bearer token-{account_id}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {"used_percent": 20, "reset_at": 1_900_000_000}
            }
        })))
        .mount(&server)
        .await;

    let recovered = service
        .refresh_account(account.id())
        .await
        .expect("refresh recovered quota");

    assert!(!recovered.fact().exhausted());
    assert_eq!(
        store
            .account(account_id)
            .expect("recovered account")
            .availability(),
        AccountAvailability::Ready
    );
}

#[tokio::test]
async fn passive_rate_limit_headers_replace_stale_secondary_window() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_passive_quota_snapshot").await;
    let account = store
        .account("acct_passive_quota_snapshot")
        .expect("account");
    let baseline = json!({
        "rate_limit": {
            "primary_window": {
                "used_percent": 91,
                "reset_at": 1_900_000_000,
                "limit_window_seconds": 2_592_000
            },
            "secondary_window": {
                "used_percent": 43,
                "reset_at": 1_900_000_000,
                "limit_window_seconds": 2_592_000
            }
        }
    });
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: Some(OpaqueProviderData::new(
                baseline.as_object().expect("quota object").clone(),
            )),
            observed_at: Some(SystemTime::now()),
        })
        .await
        .expect("persist baseline quota");
    let service = quota_service(&store);
    let headers = vec![
        ("x-codex-active-limit".to_owned(), "codex".to_owned()),
        ("x-codex-primary-used-percent".to_owned(), "92".to_owned()),
        (
            "x-codex-primary-window-minutes".to_owned(),
            "43200".to_owned(),
        ),
        (
            "x-codex-primary-reset-at".to_owned(),
            "1900000000".to_owned(),
        ),
    ];

    assert!(
        service
            .synchronize_passive_headers(&account, &headers)
            .await
            .expect("passive quota")
    );
    let snapshot = service
        .read_account(account.id())
        .await
        .expect("read quota")
        .expect("quota snapshot");
    let monthly = snapshot
        .windows()
        .iter()
        .filter(|window| window.kind() == CodexQuotaWindowKind::Monthly)
        .collect::<Vec<_>>();

    assert_eq!(monthly.len(), 1);
    assert_eq!(monthly[0].used_percent(), Some(92.0));
}

#[tokio::test]
async fn synchronize_without_accounts_is_a_noop_before_network_io() {
    let store = Arc::new(MemoryAccountStore::default());

    let summary = quota_service(&store)
        .synchronize()
        .await
        .expect("empty synchronization");

    assert_eq!(summary.updated, 0);
    assert_eq!(summary.exhausted, 0);
    assert_eq!(summary.invalid, 0);
    assert_eq!(summary.cooldown, 0);
    assert_eq!(summary.transient, 0);
    assert_eq!(summary.stale, 0);
}

#[tokio::test]
async fn periodic_quota_synchronization_skips_fresh_ready_accounts() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_periodic_ready").await;
    let account = store
        .account("acct_periodic_ready")
        .expect("created account");
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: Some(OpaqueProviderData::new(
                json!({
                    "rate_limit": {
                        "allowed": true,
                        "primary_window": {"used_percent": 10, "reset_at": 1_900_000_000}
                    }
                })
                .as_object()
                .expect("quota object")
                .clone(),
            )),
            observed_at: Some(SystemTime::now()),
        })
        .await
        .expect("persist fresh quota");

    let summary = blocked_network_quota_service(&store)
        .synchronize()
        .await
        .expect("fresh ready account must not query upstream");

    assert_eq!(summary, CodexQuotaSyncSummary::default());
}

#[tokio::test]
async fn periodic_quota_synchronization_repairs_cooldown_with_active_exhausted_quota() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_periodic_stuck_cooldown").await;
    let account = store
        .account("acct_periodic_stuck_cooldown")
        .expect("created account");
    let reset_at = Utc::now().timestamp() + 24 * 60 * 60;
    store
        .apply_state_change(AccountStateChange {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::Cooldown,
            reason: Some("rate_limited".to_owned()),
            cooldown_until: SystemTime::now().checked_add(Duration::from_secs(24 * 60 * 60)),
            observed_at: SystemTime::now(),
        })
        .await
        .expect("mark account cooldown");
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: Some(OpaqueProviderData::new(
                json!({
                    "rate_limit": {
                        "allowed": false,
                        "primary_window": {"used_percent": 100, "reset_at": reset_at}
                    }
                })
                .as_object()
                .expect("quota object")
                .clone(),
            )),
            observed_at: Some(SystemTime::now()),
        })
        .await
        .expect("persist exhausted quota");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
    );

    service.synchronize().await.expect("quota synchronization");

    assert_eq!(
        store
            .account("acct_periodic_stuck_cooldown")
            .expect("reconciled account")
            .availability(),
        AccountAvailability::QuotaExhausted
    );
}

#[tokio::test]
async fn periodic_quota_synchronization_does_not_downgrade_exhaustion_on_rate_limit() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_periodic_exhausted").await;
    let account = store
        .account("acct_periodic_exhausted")
        .expect("created account");
    store
        .apply_state_change(AccountStateChange {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::QuotaExhausted,
            reason: Some("quota_exhausted".to_owned()),
            cooldown_until: None,
            observed_at: SystemTime::now(),
        })
        .await
        .expect("mark account exhausted");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
    );

    service.synchronize().await.expect("quota synchronization");

    assert_eq!(
        store
            .account("acct_periodic_exhausted")
            .expect("persisted account")
            .availability(),
        AccountAvailability::QuotaExhausted
    );
}

#[tokio::test]
async fn periodic_quota_synchronization_skips_ordinary_cooldown_without_exhausted_quota() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_periodic_temporary_cooldown").await;
    let account = store
        .account("acct_periodic_temporary_cooldown")
        .expect("created account");
    store
        .apply_state_change(AccountStateChange {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::Cooldown,
            reason: Some("rate_limited".to_owned()),
            cooldown_until: SystemTime::now().checked_add(Duration::from_secs(60)),
            observed_at: SystemTime::now(),
        })
        .await
        .expect("mark account cooldown");

    let summary = blocked_network_quota_service(&store)
        .synchronize()
        .await
        .expect("ordinary cooldown must not query upstream");

    assert_eq!(summary, CodexQuotaSyncSummary::default());
    assert_eq!(
        store
            .account("acct_periodic_temporary_cooldown")
            .expect("cooldown account")
            .availability(),
        AccountAvailability::Cooldown
    );
}

#[tokio::test]
async fn periodic_quota_synchronization_limits_stale_ready_account_scans() {
    let store = Arc::new(MemoryAccountStore::default());
    for index in 0..9 {
        let account_id = format!("acct_periodic_stale_ready_{index}");
        create_account(&store, &account_id).await;
        let account = store.account(&account_id).expect("created account");
        store
            .compare_and_swap_quota(QuotaObservation {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                quota: Some(OpaqueProviderData::new(
                    json!({
                        "rate_limit": {
                            "allowed": true,
                            "primary_window": {"used_percent": 10, "reset_at": 1_900_000_000}
                        }
                    })
                    .as_object()
                    .expect("quota object")
                    .clone(),
                )),
                observed_at: Some(SystemTime::UNIX_EPOCH),
            })
            .await
            .expect("persist stale quota");
    }
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": false,
                "primary_window": {"used_percent": 100, "reset_at": 1_900_000_000}
            }
        })))
        .mount(&server)
        .await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
    );

    let summary = service.synchronize().await.expect("quota synchronization");

    assert_eq!(summary.exhausted, 8);
    assert_eq!(
        store
            .account("acct_periodic_stale_ready_0")
            .expect("scanned account")
            .availability(),
        AccountAvailability::QuotaExhausted
    );
    assert_eq!(
        store
            .account("acct_periodic_stale_ready_8")
            .expect("deferred account")
            .availability(),
        AccountAvailability::Ready
    );
}

#[tokio::test]
async fn periodic_quota_synchronization_attempts_quota_exhausted_accounts() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_periodic_exhausted").await;
    let account = store
        .account("acct_periodic_exhausted")
        .expect("created account");
    store
        .apply_state_change(AccountStateChange {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::QuotaExhausted,
            reason: Some("quota_exhausted".to_owned()),
            cooldown_until: None,
            observed_at: SystemTime::now(),
        })
        .await
        .expect("mark account exhausted");

    let summary = blocked_network_quota_service(&store)
        .synchronize()
        .await
        .expect("quota synchronization");

    assert_eq!(summary.transient, 1);
}

#[tokio::test]
async fn periodic_quota_synchronization_throttles_the_same_exhausted_account() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_periodic_throttled").await;
    let account = store
        .account("acct_periodic_throttled")
        .expect("created account");
    store
        .apply_state_change(AccountStateChange {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::QuotaExhausted,
            reason: Some("quota_exhausted".to_owned()),
            cooldown_until: None,
            observed_at: SystemTime::now(),
        })
        .await
        .expect("mark account exhausted");
    let service = blocked_network_quota_service(&store);

    service.synchronize().await.expect("first quota cycle");
    let summary = service.synchronize().await.expect("throttled quota cycle");

    assert_eq!(summary, CodexQuotaSyncSummary::default());
}

#[tokio::test]
async fn periodic_quota_synchronization_rechecks_an_exhausted_account_once_when_its_reset_is_due() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_periodic_reset_due").await;
    let account = store
        .account("acct_periodic_reset_due")
        .expect("created account");
    store
        .apply_state_change(AccountStateChange {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::QuotaExhausted,
            reason: Some("quota_exhausted".to_owned()),
            cooldown_until: None,
            observed_at: SystemTime::now(),
        })
        .await
        .expect("mark account exhausted");
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: Some(OpaqueProviderData::new(
                json!({
                    "rate_limit": {
                        "allowed": false,
                        "primary_window": {"used_percent": 100, "reset_at": 1_900_000_000}
                    }
                })
                .as_object()
                .expect("quota object")
                .clone(),
            )),
            observed_at: Some(SystemTime::now()),
        })
        .await
        .expect("persist future reset");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
    );

    let first = service.synchronize().await.expect("initial refresh");
    assert_eq!(first.transient, 1);
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: Some(OpaqueProviderData::new(
                json!({
                    "rate_limit": {
                        "allowed": false,
                        "primary_window": {"used_percent": 100, "reset_at": 1}
                    }
                })
                .as_object()
                .expect("quota object")
                .clone(),
            )),
            observed_at: Some(SystemTime::now()),
        })
        .await
        .expect("persist due reset");

    let reset_recheck = service.synchronize().await.expect("reset recheck");
    let repeated = service
        .synchronize()
        .await
        .expect("throttled reset recheck");

    assert_eq!(reset_recheck.transient, 1);
    assert_eq!(repeated, CodexQuotaSyncSummary::default());
}

#[tokio::test]
async fn invalid_quota_json_for_one_account_does_not_abort_the_batch() {
    let store = Arc::new(MemoryAccountStore::default());
    for account_id in ["acct_quota_batch_bad", "acct_quota_batch_good"] {
        create_account(&store, account_id).await;
        let account = store.account(account_id).expect("created account");
        store
            .apply_state_change(AccountStateChange {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                availability: AccountAvailability::QuotaExhausted,
                reason: Some("quota_exhausted".to_owned()),
                cooldown_until: None,
                observed_at: SystemTime::now(),
            })
            .await
            .expect("mark account exhausted");
    }
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer token-acct_quota_batch_bad"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"rate_limit": {"allowed": "not-a-boolean"}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header(
            "authorization",
            "Bearer token-acct_quota_batch_good",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": true,
                "primary_window": {"used_percent": 25.0, "reset_at": 1_900_000_000}
            }
        })))
        .mount(&server)
        .await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
    );

    let summary = service
        .synchronize()
        .await
        .expect("batch must survive one invalid account");

    assert_eq!(summary.updated, 1);
    assert_eq!(summary.transient, 1);
    let good = store
        .account("acct_quota_batch_good")
        .expect("good account");
    let snapshot = service
        .read_account(good.id())
        .await
        .expect("read good quota")
        .expect("good quota persisted");
    assert_eq!(snapshot.fact().remaining_percent(), Some(75));
}
