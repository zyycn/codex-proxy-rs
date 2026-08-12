//! OpenAI 额度事实边界与展示快照回归。

mod recovery;
mod snapshot;

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{TimeZone as _, Utc};
use gateway_core::engine::credential::{
    AccountStateChange, AccountStatus, CredentialState, ProviderAccount, ProviderAccountStore as _,
    QuotaAccessChange, QuotaAccessState, QuotaEvidence, QuotaState,
};
use provider_openai::OFFICIAL_CODEX_BASE_URL;
use provider_openai::credential::{
    CodexCredentialQuotaError, CodexCredentialQuotaService, ImportCodexOAuthCredential,
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
    quota_service_with_base_url(
        store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        OFFICIAL_CODEX_BASE_URL.to_owned(),
    )
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

async fn persist_credential_state(
    store: &MemoryAccountStore,
    account: &ProviderAccount,
    credential_state: CredentialState,
) {
    store
        .apply_state_change(AccountStateChange {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            credential_state,
            observed_at: SystemTime::now(),
            error_reason: credential_state.error_reason(),
            message: None,
        })
        .await
        .expect("persist credential state");
}

async fn persist_quota_state(
    store: &MemoryAccountStore,
    account: &ProviderAccount,
    state: QuotaState,
) {
    store
        .apply_quota_access(QuotaAccessChange {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            state,
        })
        .await
        .expect("persist quota state");
}

fn exhausted_quota(reset_at: Option<SystemTime>) -> QuotaState {
    QuotaState::exhausted(
        QuotaEvidence::UsageLimitReached,
        SystemTime::now(),
        reset_at,
    )
}

#[tokio::test]
async fn successful_inference_headers_are_authoritative_even_at_full_display_usage() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_passive_allowed";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("account");
    persist_quota_state(&store, &account, exhausted_quota(None)).await;
    let service = quota_service(&store);
    let headers = vec![
        ("x-codex-active-limit".to_owned(), "codex".to_owned()),
        ("x-codex-primary-used-percent".to_owned(), "100".to_owned()),
        ("x-codex-limit-reached".to_owned(), "true".to_owned()),
    ];

    assert!(
        service
            .synchronize_passive_headers(&account, &headers)
            .await
            .expect("passive quota")
    );
    let current = store.account(account_id).expect("updated account");
    let snapshot = service
        .read_account(account.id())
        .await
        .expect("read quota")
        .expect("quota snapshot");

    assert_eq!(snapshot.fact().remaining_percent(), Some(0));
    assert_eq!(snapshot.quota().access(), QuotaAccessState::Allowed);
    assert_eq!(current.quota().access(), QuotaAccessState::Allowed);
    assert_eq!(
        current.status_projection(SystemTime::now(), None).status,
        AccountStatus::Normal
    );
}

#[tokio::test]
async fn quota_refresh_persists_explicit_provider_denial() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_explicit_quota_denial";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("account");
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
                "primary_window": {"used_percent": 8, "reset_at": 1_900_000_000}
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

    let snapshot = service
        .refresh_account(account.id())
        .await
        .expect("refresh quota");
    let current = store.account(account_id).expect("updated account");

    assert_eq!(snapshot.fact().remaining_percent(), Some(92));
    assert_eq!(snapshot.quota().access(), QuotaAccessState::Exhausted);
    assert_eq!(
        snapshot.quota().evidence(),
        Some(QuotaEvidence::ProviderDenied)
    );
    assert_eq!(
        current.status_projection(SystemTime::now(), None).status,
        AccountStatus::QuotaExhausted
    );
}

#[tokio::test]
async fn quota_endpoint_auth_rejections_do_not_reclassify_credentials_or_quota() {
    for status in [401, 403, 429, 503] {
        let store = Arc::new(MemoryAccountStore::default());
        let account_id = format!("acct_quota_rejection_{status}");
        create_account(&store, &account_id).await;
        let account = store.account(&account_id).expect("account");
        persist_quota_state(&store, &account, exhausted_quota(None)).await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
            .respond_with(ResponseTemplate::new(status))
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
        let current = store.account(&account_id).expect("preserved account");
        assert_eq!(current.credential_state(), CredentialState::Ready);
        assert_eq!(current.quota().access(), QuotaAccessState::Exhausted);
    }
}

#[tokio::test]
async fn payment_required_is_authoritative_quota_exhaustion_without_fabricated_usage() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_payment_required";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("account");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "detail": {"code": "billing_required"}
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

    assert!(service.refresh_account(account.id()).await.is_err());
    let current = store.account(account_id).expect("updated account");
    assert_eq!(current.quota().access(), QuotaAccessState::Exhausted);
    assert_eq!(
        current.quota().evidence(),
        Some(QuotaEvidence::PaymentRequired)
    );
    assert!(!store.has_quota(account_id));
}

#[tokio::test]
async fn quota_refresh_preserves_disabled_and_credential_error_facts() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_disabled_credential_error";
    create_account_with_enabled(&store, account_id, false).await;
    let account = store.account(account_id).expect("account");
    persist_credential_state(&store, &account, CredentialState::Expired).await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {"allowed": true, "primary_window": {"used_percent": 1}}
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
        .expect("refresh quota");
    let current = store.account(account_id).expect("updated account");

    assert!(!current.enabled());
    assert_eq!(current.credential_state(), CredentialState::Expired);
    assert_eq!(current.quota().access(), QuotaAccessState::Allowed);
    assert_eq!(
        current.status_projection(SystemTime::now(), None).status,
        AccountStatus::Disabled
    );
}
