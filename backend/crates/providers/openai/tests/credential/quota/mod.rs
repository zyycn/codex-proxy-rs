//! quota 测试入口与共享 fixture。
//!
//! - [`snapshot`]：快照聚合、窗口滚动与调度信号回归。
//! - [`recovery`]：402 恢复证据回归。

mod recovery;
mod snapshot;

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{TimeZone as _, Utc};
use gateway_core::engine::credential::{
    AccountAvailability, AccountStateChange, OpaqueProviderData, ProviderAccountStore as _,
    QuotaObservation,
};
use provider_openai::OFFICIAL_CODEX_BASE_URL;
use provider_openai::credential::{
    CodexCredentialQuotaError, CodexCredentialQuotaService, CodexQuotaSyncSummary,
    CodexQuotaWindowKind, ImportCodexOAuthCredential,
};
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{MemoryAccountStore, profile, secret};

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
        Arc::new(crate::support::MemoryCooldownPort::new()),
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
    create_account_with_enabled(store, account_id, true).await;
}

async fn create_account_with_enabled(
    store: &Arc<MemoryAccountStore>,
    account_id: &str,
    enabled: bool,
) {
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: account_id.to_owned(),
            name: account_id.to_owned(),
            secret: secret(&format!("token-{account_id}")),
            verified_account: profile(&format!("chatgpt-{account_id}")),
            next_refresh_at: Some(Utc::now() + chrono::Duration::minutes(30)),
            enabled,
        })
        .await;
}

#[tokio::test]
async fn passive_rate_limit_headers_update_quota_with_revision_fence() {
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
    // 被动头限流只写 quota，不改变 availability（限流不改变账号可用性）。
    assert_eq!(current.availability(), AccountAvailability::Ready);
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
async fn passive_full_percent_with_explicit_allowance_keeps_account_ready() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_passive_full_allowed").await;
    let account = store.account("acct_passive_full_allowed").expect("account");
    let service = quota_service(&store);
    let headers = vec![
        ("x-codex-active-limit".to_owned(), "codex".to_owned()),
        ("x-codex-primary-used-percent".to_owned(), "100".to_owned()),
        (
            "x-codex-primary-reset-at".to_owned(),
            "1900000000".to_owned(),
        ),
        ("x-codex-allowed".to_owned(), "true".to_owned()),
        ("x-codex-limit-reached".to_owned(), "false".to_owned()),
    ];

    assert!(
        service
            .synchronize_passive_headers(&account, &headers)
            .await
            .expect("passive quota")
    );
    let current = store
        .account("acct_passive_full_allowed")
        .expect("current account");
    let snapshot = service
        .read_account(account.id())
        .await
        .expect("read quota")
        .expect("quota snapshot");

    assert_eq!(
        (
            current.availability(),
            snapshot.fact().remaining_percent(),
            snapshot.fact().exhausted(),
        ),
        // used=100 是快照级 exhaustion，但 availability 保持 Ready
        // （限流不改变账号可用性）。
        (AccountAvailability::Ready, Some(0), true)
    );
}

#[tokio::test]
async fn passive_headers_should_materialize_false_when_quota_is_not_exhausted() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_passive_quota_available").await;
    let account = store
        .account("acct_passive_quota_available")
        .expect("account");
    let service = quota_service(&store);
    let headers = vec![
        ("x-codex-active-limit".to_owned(), "codex".to_owned()),
        ("x-codex-primary-used-percent".to_owned(), "31".to_owned()),
        ("x-codex-allowed".to_owned(), "true".to_owned()),
        ("x-codex-limit-reached".to_owned(), "false".to_owned()),
    ];

    assert!(
        service
            .synchronize_passive_headers(&account, &headers)
            .await
            .expect("passive quota")
    );

    let observations = store
        .get_quotas(std::slice::from_ref(account.id()))
        .await
        .expect("read quota observation");
    assert_eq!(
        observations
            .first()
            .and_then(|observation| observation.limit_reached),
        Some(false)
    );
}

#[tokio::test]
async fn manual_quota_refresh_preserves_disabled_account_state() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_disabled_quota_state";
    create_account_with_enabled(&store, account_id, false).await;
    let account = store.account(account_id).expect("created disabled account");
    store
        .apply_state_change(AccountStateChange {
            message: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::Expired,
            observed_at: SystemTime::now(),
        })
        .await
        .expect("seed disabled account state");
    let server = MockServer::start().await;
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
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
    );

    service
        .refresh_account(account.id())
        .await
        .expect("refresh disabled account quota");

    let current = store
        .account(account_id)
        .expect("disabled account after quota refresh");
    assert!(!current.enabled());
    assert_eq!(current.availability(), AccountAvailability::Expired);
    assert!(store.has_quota(account_id));
}

#[tokio::test]
async fn manual_quota_auth_rejection_does_not_invalidate_refreshable_credential() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_manual_quota_auth_rejected";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("created account");
    store
        .apply_state_change(AccountStateChange {
            message: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::QuotaExhausted,
            observed_at: SystemTime::now(),
        })
        .await
        .expect("mark account exhausted");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(401))
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

    assert!(matches!(
        service.refresh_account(account.id()).await,
        Err(CodexCredentialQuotaError::Upstream { .. })
    ));
    assert_eq!(
        store
            .account(account_id)
            .expect("preserved account")
            .availability(),
        AccountAvailability::QuotaExhausted
    );
}

#[tokio::test]
async fn manual_quota_refresh_waits_for_expired_oauth_token_to_be_refreshed() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_manual_quota_expired_token";
    let mut expired_profile = profile(&format!("chatgpt-{account_id}"));
    expired_profile.access_token_expires_at = Some(Utc::now() - chrono::Duration::minutes(1));
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: account_id.to_owned(),
            name: account_id.to_owned(),
            secret: secret(&format!("token-{account_id}")),
            verified_account: expired_profile,
            next_refresh_at: Some(Utc::now() - chrono::Duration::minutes(5)),
            enabled: true,
        })
        .await;
    let account = store.account(account_id).expect("created account");
    let server = MockServer::start().await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
    );

    assert!(matches!(
        service.refresh_account(account.id()).await,
        Err(CodexCredentialQuotaError::CredentialRefreshRequired)
    ));
    assert!(
        server
            .received_requests()
            .await
            .expect("received requests")
            .is_empty()
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
            limit_reached: None,
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
async fn passive_active_limit_replaces_stale_codex_alias() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_passive_active_limit").await;
    let account = store.account("acct_passive_active_limit").expect("account");
    let baseline = json!({
        "additional_rate_limits": [{
            "metered_feature": "codex",
            "rate_limit": {
                "allowed": true,
                "primary_window": {
                    "used_percent": 20,
                    "reset_at": 1_900_000_000,
                    "limit_window_seconds": 604_800
                }
            }
        }]
    });
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: Some(OpaqueProviderData::new(
                baseline.as_object().expect("quota object").clone(),
            )),
            observed_at: Some(SystemTime::now()),
            limit_reached: None,
        })
        .await
        .expect("persist baseline quota");
    let service = quota_service(&store);
    let headers = vec![
        ("x-codex-active-limit".to_owned(), "premium".to_owned()),
        ("x-premium-primary-used-percent".to_owned(), "55".to_owned()),
        (
            "x-premium-primary-window-minutes".to_owned(),
            "10080".to_owned(),
        ),
        (
            "x-premium-primary-reset-at".to_owned(),
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

    assert_eq!(snapshot.windows().len(), 1);
    assert_eq!(snapshot.windows()[0].used_percent(), Some(55.0));
}

#[tokio::test]
async fn passive_metadata_headers_preserve_quota_observation_time() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_passive_metadata").await;
    let account = store.account("acct_passive_metadata").expect("account");
    let baseline = json!({
        "rate_limit": {
            "allowed": true,
            "primary_window": {
                "used_percent": 20,
                "reset_at": 1_900_000_000,
                "limit_window_seconds": 604_800
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
            limit_reached: None,
        })
        .await
        .expect("persist baseline quota");
    let before = store
        .get_quotas(std::slice::from_ref(account.id()))
        .await
        .expect("read baseline quota");
    let service = quota_service(&store);
    let headers = vec![
        ("x-codex-plan-type".to_owned(), "free".to_owned()),
        ("x-codex-credits-has-credits".to_owned(), "true".to_owned()),
        ("x-codex-credits-unlimited".to_owned(), "false".to_owned()),
    ];

    assert!(
        service
            .synchronize_passive_headers(&account, &headers)
            .await
            .expect("metadata-only passive response")
    );
    let after = store
        .get_quotas(std::slice::from_ref(account.id()))
        .await
        .expect("read updated quota metadata");

    assert_eq!(after[0].observed_at, before[0].observed_at);
    assert_eq!(after[0].limit_reached, before[0].limit_reached);
    assert_eq!(
        store
            .quota_json("acct_passive_metadata")
            .and_then(|quota| quota.get("plan_type").cloned()),
        Some(json!("free"))
    );
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
    assert_eq!(summary.banned, 0);
    assert_eq!(summary.transient, 0);
    assert_eq!(summary.stale, 0);
}

#[tokio::test]
async fn periodic_quota_synchronization_does_not_scan_stale_ready_accounts() {
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
            // 正常账号即使缓存已旧，也只等待真实请求被动同步，不能被定时任务主动拉取。
            observed_at: Some(SystemTime::UNIX_EPOCH),
            limit_reached: None,
        })
        .await
        .expect("persist stale quota");

    let summary = blocked_network_quota_service(&store)
        .synchronize()
        .await
        .expect("ready account must not query upstream");

    assert_eq!(summary, CodexQuotaSyncSummary::default());
}

#[tokio::test]
async fn periodic_quota_synchronization_preserves_availability_when_usage_peaks() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_periodic_stuck_cooldown").await;
    let account = store
        .account("acct_periodic_stuck_cooldown")
        .expect("created account");
    let reset_at = Utc::now().timestamp() + 24 * 60 * 60;
    store
        .apply_state_change(AccountStateChange {
            message: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::Ready,
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
            limit_reached: None,
        })
        .await
        .expect("persist exhausted quota");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": false,
                "limit_reached": true,
                "primary_window": {"used_percent": 100, "reset_at": reset_at}
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

    service.synchronize().await.expect("quota synchronization");

    // 429/触顶不再写入 availability（限流不改变账号可用性），worker 只更新 quota 数据。
    let reconciled = store
        .account("acct_periodic_stuck_cooldown")
        .expect("reconciled account");
    assert_eq!(reconciled.availability(), AccountAvailability::Ready);
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
            message: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::QuotaExhausted,
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

    let account = store
        .account("acct_periodic_exhausted")
        .expect("persisted account");
    assert_eq!(account.availability(), AccountAvailability::QuotaExhausted);
}

#[tokio::test]
async fn periodic_quota_synchronization_respects_retry_after_on_service_unavailable() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_periodic_503_retry_after").await;
    let account = store
        .account("acct_periodic_503_retry_after")
        .expect("created account");
    store
        .apply_state_change(AccountStateChange {
            message: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::QuotaExhausted,
            observed_at: SystemTime::now(),
        })
        .await
        .expect("mark account exhausted");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        // 5xx 短退避重试后仍失败，Retry-After 用于写冷却，不再落入"未分类"。
        .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "120"))
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

    let account = store
        .account("acct_periodic_503_retry_after")
        .expect("persisted account");
    assert_eq!(account.availability(), AccountAvailability::QuotaExhausted);
}

#[tokio::test]
async fn periodic_quota_synchronization_skips_ready_accounts_without_quota_reached() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_periodic_temporary_cooldown").await;
    let account = store
        .account("acct_periodic_temporary_cooldown")
        .expect("created account");
    store
        .apply_state_change(AccountStateChange {
            message: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::Ready,
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
            message: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::QuotaExhausted,
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
async fn periodic_quota_synchronization_rechecks_exhausted_accounts_without_waiting_for_reset() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_periodic_exhausted_cooldown").await;
    let account = store
        .account("acct_periodic_exhausted_cooldown")
        .expect("created account");
    store
        .apply_state_change(AccountStateChange {
            message: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::QuotaExhausted,
            observed_at: SystemTime::now(),
        })
        .await
        .expect("mark account exhausted with a future reset");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(500))
        // 5xx 短退避重试 1s/2s 后仍失败，最终计入 transient。
        .expect(3)
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

    // reset 仅用于展示/调度状态，不能推迟额度耗尽账号的定时复核。
    let due = service.synchronize().await.expect("quota synchronization");
    let repeated = service
        .synchronize()
        .await
        .expect("quota synchronization repeated");

    assert_eq!(due.transient, 1);
    assert_eq!(repeated, CodexQuotaSyncSummary::default());
}

#[tokio::test]
async fn periodic_quota_synchronization_does_not_recover_from_percent_drop_before_reset() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_periodic_recovery_evidence";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("created account");
    let reset_at = 1_900_000_000;
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: Some(OpaqueProviderData::new(
                json!({
                    "rate_limit": {
                        "allowed": true,
                        "limit_reached": false,
                        "primary_window": {"used_percent": 100, "reset_at": reset_at}
                    }
                })
                .as_object()
                .expect("quota object")
                .clone(),
            )),
            observed_at: Some(SystemTime::now()),
            limit_reached: None,
        })
        .await
        .expect("seed post-failure quota snapshot");
    store
        .apply_state_change(AccountStateChange {
            message: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::QuotaExhausted,
            observed_at: SystemTime::now(),
        })
        .await
        .expect("mark account exhausted");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {"used_percent": 100, "reset_at": reset_at}
            }
        })))
        .expect(1)
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

    let unchanged = service
        .synchronize()
        .await
        .expect("unchanged quota recheck");

    // used=100 是快照级 exhaustion，计 exhausted 而非 updated。
    assert_eq!(unchanged.exhausted, 1);
    assert_eq!(
        store
            .account(account_id)
            .expect("account after unchanged snapshot")
            .availability(),
        AccountAvailability::QuotaExhausted
    );

    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                // 无 allowed/limit_reached 标记的模糊快照。
                "primary_window": {"used_percent": 99, "reset_at": reset_at}
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    // 新 worker 实例模拟下一次 30 分钟复核，避免测试等待真实节流周期。
    let next_cycle = quota_service_with_base_url(
        &store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
    );

    let lower_percent = next_cycle
        .synchronize()
        .await
        .expect("lower percent quota recheck");

    assert_eq!(lower_percent.updated, 1);
    assert_eq!(
        store
            .account(account_id)
            .expect("account after lower percent snapshot")
            .availability(),
        AccountAvailability::QuotaExhausted,
        "a provider percentage change is not sufficient to release a confirmed exhaustion"
    );
}

#[tokio::test]
async fn periodic_quota_synchronization_recovers_after_recorded_reset() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_periodic_reset_elapsed";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("created account");
    store
        .apply_state_change(AccountStateChange {
            message: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::QuotaExhausted,
            observed_at: SystemTime::now(),
        })
        .await
        .expect("mark account exhausted with an elapsed reset");
    let server = MockServer::start().await;
    let elapsed_reset = Utc::now().timestamp() - 1;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {"used_percent": 1, "reset_at": elapsed_reset}
            }
        })))
        .expect(1)
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

    // 定时复核可提前执行，但只有已记录的 reset 到期后才释放额度耗尽状态。
    let summary = service.synchronize().await.expect("quota synchronization");

    assert_eq!(summary.updated, 1);
    let current = store
        .account(account_id)
        .expect("account after elapsed reset");
    assert_eq!(current.availability(), AccountAvailability::Ready);
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
            message: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::QuotaExhausted,
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
async fn periodic_quota_synchronization_does_not_bypass_throttle_for_a_new_reset() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_periodic_new_reset";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("created account");
    store
        .apply_state_change(AccountStateChange {
            message: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::QuotaExhausted,
            observed_at: SystemTime::now(),
        })
        .await
        .expect("mark account exhausted with due reset");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": false,
                "limit_reached": true,
                "primary_window": {"used_percent": 100, "reset_at": 2}
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

    let first = service.synchronize().await.expect("first reset recheck");
    let second = service
        .synchronize()
        .await
        .expect("throttled reset recheck");
    let repeated = service.synchronize().await.expect("same reset throttled");
    let request_count = server
        .received_requests()
        .await
        .expect("received requests")
        .len();

    assert_eq!(
        (first.exhausted, second.exhausted, repeated, request_count,),
        (1, 0, CodexQuotaSyncSummary::default(), 1,)
    );
}

#[tokio::test]
async fn invalid_quota_json_for_one_account_does_not_abort_the_batch() {
    let store = Arc::new(MemoryAccountStore::default());
    for account_id in ["acct_quota_batch_bad", "acct_quota_batch_good"] {
        create_account(&store, account_id).await;
        let account = store.account(account_id).expect("created account");
        store
            .apply_state_change(AccountStateChange {
                message: None,
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                availability: AccountAvailability::QuotaExhausted,
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
