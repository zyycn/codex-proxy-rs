use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{TimeZone, Utc};
use futures::future::join_all;
use gateway_core::engine::credential::{
    AccountAvailability, AccountStateChange, ProviderAccountStore,
};
use gateway_core::routing::{InstanceHealth, ProviderInstance, ProviderKind};
use provider_openai::credential::{
    CodexCredentialQuotaService, CodexQuotaSyncSummary, CodexQuotaWindowKind,
    ImportCodexOAuthCredential, parse_codex_quota_usage,
};
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{MemoryAccountStore, instance_id, loopback_origin_policy, profile, secret};

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
        terminal: "xterm".to_owned(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 7, 18, 0, 0, 0)
            .single()
            .expect("fixture time"),
    })
}

fn quota_service(store: &Arc<MemoryAccountStore>) -> CodexCredentialQuotaService {
    CodexCredentialQuotaService::new(
        store.repository(),
        wire_profile(),
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        loopback_origin_policy(),
    )
}

#[tokio::test]
async fn concurrent_cold_scheduling_hydration_reads_quota_once() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_hydration".to_owned(),
            provider_instance_id: instance_id().to_string(),
            name: "hydration".to_owned(),
            secret: secret("hydration-token"),
            verified_account: profile("chatgpt-acct_hydration"),
            next_refresh_at: Some(Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let account = store.account("acct_hydration").expect("created account");
    let service = quota_service(&store);

    join_all((0..32).map(|_| service.prepare_scheduling(std::slice::from_ref(&account)))).await;

    assert_eq!(store.quota_reads(), 1);
}

#[tokio::test]
async fn quota_service_stores_raw_provider_json_and_projects_common_state() {
    let server = MockServer::start().await;
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
            },
            "provider_specific_future_window": {"used": "opaque"}
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
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .and(header("authorization", "Bearer quota-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&raw))
        .expect(2)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_quota".to_owned(),
            provider_instance_id: instance_id().to_string(),
            name: "quota".to_owned(),
            secret: secret("quota-token"),
            verified_account: profile("chatgpt-acct_quota"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let service = quota_service(&store);
    let instance = ProviderInstance::new(
        instance_id(),
        ProviderKind::new("openai").expect("provider"),
        format!("{}/backend-api", server.uri()),
        true,
        InstanceHealth::Healthy,
    );
    let summary = service
        .synchronize_instance(&instance)
        .await
        .expect("synchronize quota");
    assert_eq!(summary.updated, 1, "unexpected quota summary: {summary:?}");
    let account = store.account("acct_quota").expect("account");
    assert_eq!(account.availability(), AccountAvailability::Ready);
    let parsed = service
        .read_account(account.id())
        .await
        .expect("read quota")
        .expect("quota snapshot");
    assert_eq!(parsed.account_id(), account.id());
    assert_eq!(parsed.credential_revision(), account.revision());
    assert_eq!(parsed.windows().len(), 3);
    assert_eq!(parsed.windows()[0].kind(), CodexQuotaWindowKind::ShortTerm);
    assert_eq!(parsed.windows()[0].window_seconds(), Some(18_000));
    assert_eq!(parsed.windows()[0].source(), "core");
    assert_eq!(parsed.windows()[1].kind(), CodexQuotaWindowKind::Weekly);
    assert_eq!(parsed.windows()[1].window_seconds(), Some(604_800));
    assert_eq!(parsed.windows()[2].kind(), CodexQuotaWindowKind::Monthly);
    assert_eq!(parsed.windows()[2].source(), "spend_control");
    let refreshed = service
        .refresh_account(&instance, account.id())
        .await
        .expect("refresh one account");
    assert_eq!(refreshed.account_id(), account.id());
    assert_eq!(refreshed.windows()[0].used_percent(), Some(37.0));
    let observation = store
        .get_quotas(std::slice::from_ref(account.id()))
        .await
        .expect("quota read")
        .pop()
        .expect("quota observation");
    assert_eq!(
        observation
            .quota
            .expect("raw quota")
            .expose_to_provider()
            .get("provider_specific_root"),
        raw.get("provider_specific_root")
    );
}

#[tokio::test]
async fn quota_refresh_success_must_not_clear_newer_future_cooldown() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(100))
                .set_body_json(json!({
                    "rate_limit": {
                        "allowed": true,
                        "primary_window": {"used_percent": 10}
                    }
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_quota_cooldown".to_owned(),
            provider_instance_id: instance_id().to_string(),
            name: "quota cooldown".to_owned(),
            secret: secret("quota-cooldown-token"),
            verified_account: profile("chatgpt-acct_quota_cooldown"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let account = store.account("acct_quota_cooldown").expect("quota account");
    let service = quota_service(&store);
    let instance = ProviderInstance::new(
        instance_id(),
        ProviderKind::new("openai").expect("provider"),
        format!("{}/backend-api", server.uri()),
        true,
        InstanceHealth::Healthy,
    );
    let cooldown_until = SystemTime::now() + Duration::from_secs(3_600);
    let synchronize = service.synchronize_instance(&instance);
    let apply_newer_cooldown = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        store
            .apply_state_change(AccountStateChange {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                availability: AccountAvailability::Cooldown,
                reason: Some("model_request_rate_limited".to_owned()),
                cooldown_until: Some(cooldown_until),
                observed_at: SystemTime::now(),
            })
            .await
            .expect("apply newer cooldown");
    };

    let (summary, ()) = tokio::join!(synchronize, apply_newer_cooldown);
    assert_eq!(summary.expect("quota synchronization").updated, 1);
    let refreshed = store
        .account("acct_quota_cooldown")
        .expect("refreshed account");
    assert_eq!(refreshed.availability(), AccountAvailability::Cooldown);
    assert_eq!(refreshed.cooldown_until(), Some(cooldown_until));
}

#[tokio::test]
async fn non_exhausted_quota_refresh_should_restore_quota_exhausted_account() {
    let (summary, availability, cooldown_until) = synchronize_account_from_state(
        "acct_quota_recovered",
        AccountAvailability::QuotaExhausted,
        None,
        ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": true,
                "primary_window": {"used_percent": 10}
            }
        })),
    )
    .await;

    assert_eq!(
        (summary.updated, availability, cooldown_until),
        (1, AccountAvailability::Ready, None)
    );
}

#[tokio::test]
async fn passive_rate_limit_headers_update_quota_and_account_state_with_revision_fence() {
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: "acct_passive_quota".to_owned(),
            provider_instance_id: instance_id().to_string(),
            name: "passive quota".to_owned(),
            secret: secret("passive-quota-token"),
            verified_account: profile("chatgpt-acct_passive_quota"),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
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
    let observation = store
        .get_quotas(std::slice::from_ref(account.id()))
        .await
        .expect("quota")
        .pop()
        .expect("quota observation");
    let quota = serde_json::Value::Object(
        observation
            .quota
            .expect("quota JSON")
            .expose_to_provider()
            .clone(),
    );
    assert_eq!(
        quota
            .pointer("/rate_limit/primary_window/limit_window_seconds")
            .and_then(serde_json::Value::as_u64),
        Some(18_000)
    );
    assert_eq!(
        quota
            .pointer("/rate_limit/primary_window/reset_at")
            .and_then(serde_json::Value::as_i64),
        Some(reset_at)
    );
}

#[tokio::test]
async fn exhausted_quota_refresh_should_upgrade_future_cooldown() {
    let cooldown_until = SystemTime::now() + Duration::from_secs(3_600);
    let (summary, availability, projected_cooldown) = synchronize_account_from_state(
        "acct_quota_exhausted",
        AccountAvailability::Cooldown,
        Some(cooldown_until),
        ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": false,
                "limit_reached": true,
                "primary_window": {"used_percent": 100}
            }
        })),
    )
    .await;

    assert_eq!(
        (summary.exhausted, availability, projected_cooldown),
        (1, AccountAvailability::QuotaExhausted, None)
    );
}

#[tokio::test]
async fn structured_deactivated_workspace_rejection_should_ban_account() {
    let (summary, availability, cooldown_until) = synchronize_account_from_state(
        "acct_quota_deactivated",
        AccountAvailability::Ready,
        None,
        ResponseTemplate::new(402)
            .set_body_json(json!({"detail": {"code": "deactivated_workspace"}})),
    )
    .await;

    assert_eq!(
        (summary.invalid, availability, cooldown_until),
        (1, AccountAvailability::Banned, None)
    );
}

#[tokio::test]
async fn generic_payment_required_rejection_should_exhaust_quota() {
    let (summary, availability, cooldown_until) = synchronize_account_from_state(
        "acct_quota_payment_required",
        AccountAvailability::Ready,
        None,
        ResponseTemplate::new(402).set_body_json(json!({"detail": {"code": "payment_required"}})),
    )
    .await;

    assert_eq!(
        (summary.exhausted, availability, cooldown_until),
        (1, AccountAvailability::QuotaExhausted, None)
    );
}

async fn synchronize_account_from_state(
    account_id: &str,
    availability: AccountAvailability,
    cooldown_until: Option<SystemTime>,
    response: ResponseTemplate,
) -> (
    CodexQuotaSyncSummary,
    AccountAvailability,
    Option<SystemTime>,
) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(response)
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: account_id.to_owned(),
            provider_instance_id: instance_id().to_string(),
            name: account_id.to_owned(),
            secret: secret(&format!("token-{account_id}")),
            verified_account: profile(&format!("chatgpt-{account_id}")),
            next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
            enabled: true,
        })
        .await;
    let account = store.account(account_id).expect("quota account");
    store
        .apply_state_change(AccountStateChange {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability,
            reason: Some("previous_state".to_owned()),
            cooldown_until,
            observed_at: SystemTime::now(),
        })
        .await
        .expect("apply initial account state");
    let service = quota_service(&store);
    let instance = ProviderInstance::new(
        instance_id(),
        ProviderKind::new("openai").expect("provider"),
        format!("{}/backend-api", server.uri()),
        true,
        InstanceHealth::Healthy,
    );
    let summary = service
        .synchronize_instance(&instance)
        .await
        .expect("synchronize quota");
    let account = store.account(account_id).expect("refreshed quota account");
    (summary, account.availability(), account.cooldown_until())
}
