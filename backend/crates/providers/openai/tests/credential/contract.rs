use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use futures::executor::block_on;
use gateway_core::engine::credential::{
    AccountAvailability, AccountSelectionPolicy, ProviderAccountId, RotationStrategy,
};
use gateway_core::engine::{
    AccountAttemptContext, AttemptContext, CancellationToken, ModelRequestId,
};
use provider_openai::credential::{
    CodexAccountFailure, CodexCookiePolicy, CodexCredentialCatalogService, CodexCredentialCodec,
    CodexCredentialQuotaService, CodexCredentialSelector, CreateCodexCredential,
    CredentialSelectionError, SelectCodexCredential,
};
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use provider_openai::{CodexOriginPolicy, OfficialCodexOriginPolicy};
use secrecy::ExposeSecret;
use url::Url;

use crate::support::{
    MemoryAccountStore, TestLeaseCoordinator, account_policy, instance_id, profile, secret,
};

fn create_account(store: &Arc<MemoryAccountStore>, id: &str, token: &str) {
    block_on(
        store
            .repository()
            .create_oauth_credential(CreateCodexCredential {
                account_id: id.to_owned(),
                provider_instance_id: instance_id().to_string(),
                name: id.to_owned(),
                secret: secret(token),
                account: profile(&format!("chatgpt-{id}")),
                next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
                enabled: true,
            }),
    )
    .expect("create account");
}

fn attempt(excluded_accounts: BTreeSet<ProviderAccountId>) -> AttemptContext {
    attempt_with_required(excluded_accounts, None)
}

fn attempt_with_required(
    excluded_accounts: BTreeSet<ProviderAccountId>,
    required_account: Option<ProviderAccountId>,
) -> AttemptContext {
    AttemptContext::new(
        ModelRequestId::new("req_codex_contract").expect("request id"),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(excluded_accounts, required_account, None),
        None,
        CancellationToken::new(),
    )
}

fn round_robin_attempt() -> AttemptContext {
    AttemptContext::new(
        ModelRequestId::new("req_codex_round_robin").expect("request id"),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        AccountSelectionPolicy::new(
            RotationStrategy::RoundRobin,
            NonZeroU32::new(2).expect("concurrency"),
            Duration::ZERO,
        ),
        AccountAttemptContext::new(BTreeSet::new(), None, None),
        None,
        CancellationToken::new(),
    )
}

fn selector(
    store: &Arc<MemoryAccountStore>,
    leases: Arc<TestLeaseCoordinator>,
) -> CodexCredentialSelector {
    let profile = CodexWireProfileState::new(CodexWireProfile {
        originator: "codex_cli_rs".to_owned(),
        codex_version: "0.144.0".to_owned(),
        desktop_version: "1.0.0".to_owned(),
        desktop_build: "1".to_owned(),
        os_type: "linux".to_owned(),
        os_version: "6.8".to_owned(),
        arch: "x86_64".to_owned(),
        terminal: "selector-contract".to_owned(),
        verified_at: Utc::now(),
    });
    let http = reqwest::Client::builder().build().expect("HTTP client");
    let origin: Arc<dyn CodexOriginPolicy> = Arc::new(OfficialCodexOriginPolicy);
    let catalog = Arc::new(CodexCredentialCatalogService::new(
        store.repository(),
        profile.clone(),
        http.clone(),
        Arc::clone(&origin),
    ));
    let quota = Arc::new(CodexCredentialQuotaService::new(
        store.repository(),
        profile,
        http,
        origin,
    ));
    CodexCredentialSelector::new(
        store.repository(),
        leases,
        catalog,
        quota,
        CodexCookiePolicy::official().expect("official cookie policy"),
    )
}

#[test]
fn codec_persists_tokens_as_plaintext_provider_json() {
    let encoded = CodexCredentialCodec::encode_new(
        &secret("literal-access-token"),
        &profile("chatgpt-literal"),
        Vec::new(),
    )
    .expect("encode plaintext credential");
    assert_eq!(
        encoded
            .expose_to_provider()
            .get("access_token")
            .and_then(serde_json::Value::as_str),
        Some("literal-access-token")
    );
    assert_eq!(
        encoded
            .expose_to_provider()
            .get("refresh_token")
            .and_then(serde_json::Value::as_str),
        Some("rt-literal-access-token")
    );
    let mut keys = encoded
        .expose_to_provider()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "access_token",
            "cookies",
            "installation_id",
            "principal",
            "refresh_token",
            "schema_version",
        ]
    );
}

#[test]
fn codec_reimport_preserves_existing_installation_id_for_the_same_principal() {
    let existing = CodexCredentialCodec::encode_new(
        &secret("existing-access-token"),
        &profile("chatgpt-stable-installation"),
        Vec::new(),
    )
    .expect("existing credential");
    let incoming = CodexCredentialCodec::encode_new(
        &secret("incoming-access-token"),
        &profile("chatgpt-stable-installation"),
        Vec::new(),
    )
    .expect("incoming credential");
    let existing_id = CodexCredentialCodec::decode_complete(&existing)
        .expect("existing data")
        .installation_id;

    let preserved = CodexCredentialCodec::preserve_installation_id(&incoming, &existing)
        .expect("preserve installation ID");
    let preserved = CodexCredentialCodec::decode_complete(&preserved).expect("preserved data");

    assert_eq!(preserved.installation_id, existing_id);
    assert_eq!(preserved.access_token, "incoming-access-token");
}

#[test]
fn codec_reimport_rejects_installation_reuse_across_principals() {
    let existing = CodexCredentialCodec::encode_new(
        &secret("existing-access-token"),
        &profile("chatgpt-existing-principal"),
        Vec::new(),
    )
    .expect("existing credential");
    let incoming = CodexCredentialCodec::encode_new(
        &secret("incoming-access-token"),
        &profile("chatgpt-incoming-principal"),
        Vec::new(),
    )
    .expect("incoming credential");

    assert!(CodexCredentialCodec::preserve_installation_id(&incoming, &existing).is_err());
}

#[test]
fn repository_round_trips_plaintext_runtime_secret() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let account = store.account("acct_primary").expect("account");
    let runtime = block_on(store.repository().load_runtime_credential(&account))
        .expect("load runtime credential");
    assert_eq!(runtime.secret.access_token.expose_secret(), "at-primary");
    assert_eq!(
        runtime
            .secret
            .refresh_token
            .as_ref()
            .expect("refresh token")
            .expose_secret(),
        "rt-at-primary"
    );
}

#[test]
fn selector_uses_frozen_global_account_policy_for_lease() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let leases = Arc::new(TestLeaseCoordinator::default());
    let selector = selector(&store, Arc::clone(&leases));
    let attempt = attempt(BTreeSet::new());
    let lease =
        block_on(
            selector.select(&SelectCodexCredential {
                provider_instance_id: &instance_id(),
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
            }),
        )
        .expect("select account");

    assert_eq!(lease.account_id().as_str(), "acct_primary");
    let installation_id = lease.installation_id();
    assert_eq!(
        uuid::Uuid::parse_str(installation_id)
            .expect("installation UUID")
            .get_version_num(),
        4
    );
    let runtime = block_on(store.repository().load_runtime_credential(lease.account()))
        .expect("runtime credential");
    assert_eq!(runtime.installation_id, installation_id);
    let requests = leases.requests.lock().expect("lease requests lock");
    assert_eq!(requests[0].provider_instance_id(), &instance_id());
    assert_eq!(requests[0].account_id(), lease.account_id());
    assert_eq!(
        requests[0].credential_revision(),
        lease.account().revision()
    );
    assert_eq!(requests[0].max_concurrent().get(), 2);
    assert_eq!(requests[0].request_interval(), Duration::from_millis(10));
    assert_eq!(requests[0].deadline(), attempt.deadline());
}

#[test]
fn selector_round_robin_cursor_advances_across_requests() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "at-first");
    create_account(&store, "acct_second", "at-second");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let request_url =
        Url::parse("https://chatgpt.com/backend-api/codex/responses").expect("request URL");
    let mut selected = Vec::new();

    for _ in 0..4 {
        let attempt = round_robin_attempt();
        let lease = block_on(selector.select(&SelectCodexCredential {
            provider_instance_id: &instance_id(),
            upstream_model: "gpt-5.4",
            request_url: &request_url,
            attempt: &attempt,
        }))
        .expect("select round robin account");
        selected.push(lease.account_id().as_str().to_owned());
    }

    assert_eq!(
        selected,
        ["acct_first", "acct_second", "acct_first", "acct_second"]
    );
}

#[test]
fn selector_honors_attempt_local_account_exclusion() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "at-first");
    create_account(&store, "acct_second", "at-second");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let attempt = attempt(BTreeSet::from([
        ProviderAccountId::new("acct_first").expect("account id")
    ]));
    let lease =
        block_on(
            selector.select(&SelectCodexCredential {
                provider_instance_id: &instance_id(),
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
            }),
        )
        .expect("select non-excluded account");
    assert_eq!(lease.account_id().as_str(), "acct_second");
}

#[test]
fn selector_uses_only_the_required_account() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "at-first");
    create_account(&store, "acct_second", "at-second");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let required = ProviderAccountId::new("acct_second").expect("account id");
    let attempt = attempt_with_required(BTreeSet::new(), Some(required.clone()));
    let lease =
        block_on(
            selector.select(&SelectCodexCredential {
                provider_instance_id: &instance_id(),
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
            }),
        )
        .expect("select required account");
    assert_eq!(lease.account_id(), &required);
}

#[test]
fn unavailable_required_account_never_falls_back() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_available", "at-available");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let attempt = attempt_with_required(
        BTreeSet::new(),
        Some(ProviderAccountId::new("acct_missing").expect("account id")),
    );
    let error =
        block_on(
            selector.select(&SelectCodexCredential {
                provider_instance_id: &instance_id(),
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
            }),
        )
        .expect_err("missing required account must not fall back");
    assert!(matches!(
        error,
        CredentialSelectionError::NoEligibleCredential
    ));
}

#[test]
fn selector_returns_capacity_error_when_every_redis_lease_is_busy() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let leases = Arc::new(TestLeaseCoordinator::default());
    *leases.busy.lock().expect("lease busy lock") = true;
    let selector = selector(&store, leases);
    let attempt = attempt(BTreeSet::new());
    let error =
        block_on(
            selector.select(&SelectCodexCredential {
                provider_instance_id: &instance_id(),
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
            }),
        )
        .expect_err("busy lease must reject selection");
    assert!(matches!(
        error,
        CredentialSelectionError::CapacityUnavailable {
            retry_after: Some(_)
        }
    ));
}

#[test]
fn credential_expired_failure_marks_unified_account_expired() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let attempt = attempt(BTreeSet::new());
    let lease =
        block_on(
            selector.select(&SelectCodexCredential {
                provider_instance_id: &instance_id(),
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
            }),
        )
        .expect("select account");
    block_on(selector.record_failure(&lease, CodexAccountFailure::CredentialExpired))
        .expect("record credential expiry");
    assert_eq!(
        store
            .account("acct_primary")
            .expect("account")
            .availability(),
        AccountAvailability::Expired
    );
}

#[test]
fn identity_verification_failure_isolates_only_selected_account() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    create_account(&store, "acct_other", "at-other");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let attempt = attempt(BTreeSet::new());
    let lease =
        block_on(
            selector.select(&SelectCodexCredential {
                provider_instance_id: &instance_id(),
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
            }),
        )
        .expect("select account");

    block_on(selector.record_failure(&lease, CodexAccountFailure::IdentityVerificationRequired))
        .expect("record identity verification failure");

    assert_eq!(
        store
            .account(lease.account_id().as_str())
            .expect("selected account")
            .availability(),
        AccountAvailability::Invalid
    );
    let other = if lease.account_id().as_str() == "acct_primary" {
        "acct_other"
    } else {
        "acct_primary"
    };
    assert_eq!(
        store.account(other).expect("other account").availability(),
        AccountAvailability::Ready
    );
}

#[test]
fn cloudflare_challenge_backoff_escalates_and_success_resets_it() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let attempt = attempt(BTreeSet::new());
    let lease =
        block_on(
            selector.select(&SelectCodexCredential {
                provider_instance_id: &instance_id(),
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
            }),
        )
        .expect("select account");

    block_on(selector.record_failure(
        &lease,
        CodexAccountFailure::CloudflareChallenge { retry_after: None },
    ))
    .expect("record first challenge");
    let first = store
        .account("acct_primary")
        .expect("account")
        .cooldown_until()
        .expect("first cooldown")
        .duration_since(SystemTime::now())
        .expect("future cooldown");
    assert!(first >= Duration::from_secs(9) && first <= Duration::from_secs(10));

    block_on(selector.record_failure(
        &lease,
        CodexAccountFailure::CloudflareChallenge { retry_after: None },
    ))
    .expect("record second challenge");
    let second = store
        .account("acct_primary")
        .expect("account")
        .cooldown_until()
        .expect("second cooldown")
        .duration_since(SystemTime::now())
        .expect("future cooldown");
    assert!(second >= Duration::from_secs(29) && second <= Duration::from_secs(30));

    selector.record_success(&lease);
    block_on(selector.record_failure(
        &lease,
        CodexAccountFailure::CloudflareChallenge { retry_after: None },
    ))
    .expect("record reset challenge");
    let reset = store
        .account("acct_primary")
        .expect("account")
        .cooldown_until()
        .expect("reset cooldown")
        .duration_since(SystemTime::now())
        .expect("future cooldown");
    assert!(reset >= Duration::from_secs(9) && reset <= Duration::from_secs(10));
}

#[test]
fn repeated_cloudflare_path_block_marks_only_the_affected_account_invalid() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    create_account(&store, "acct_other", "at-other");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let attempt = attempt_with_required(
        BTreeSet::new(),
        Some(ProviderAccountId::new("acct_primary").expect("account id")),
    );
    let lease =
        block_on(
            selector.select(&SelectCodexCredential {
                provider_instance_id: &instance_id(),
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
            }),
        )
        .expect("select account");

    for _ in 0..3 {
        block_on(selector.record_failure(&lease, CodexAccountFailure::CloudflarePathBlocked))
            .expect("record path block");
    }

    assert_eq!(
        store
            .account("acct_primary")
            .expect("affected account")
            .availability(),
        AccountAvailability::Invalid
    );
    assert_eq!(
        store
            .account("acct_other")
            .expect("other account")
            .availability(),
        AccountAvailability::Ready
    );
}

#[test]
fn cloudflare_challenge_expires_provider_owned_cookies_at_cooldown_boundary() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let required = ProviderAccountId::new("acct_primary").expect("account id");
    let request_url =
        Url::parse("https://chatgpt.com/backend-api/codex/responses").expect("request URL");
    let first_attempt = attempt_with_required(BTreeSet::new(), Some(required.clone()));
    let first = block_on(selector.select(&SelectCodexCredential {
        provider_instance_id: &instance_id(),
        upstream_model: "gpt-5.4",
        request_url: &request_url,
        attempt: &first_attempt,
    }))
    .expect("select account");
    block_on(selector.capture_response_cookies(
        &first,
        &request_url,
        &["cf_clearance=old; Path=/; Domain=chatgpt.com; Secure; Max-Age=3600".to_owned()],
    ))
    .expect("capture cookie");

    let second_attempt = attempt_with_required(BTreeSet::new(), Some(required));
    let second = block_on(selector.select(&SelectCodexCredential {
        provider_instance_id: &instance_id(),
        upstream_model: "gpt-5.4",
        request_url: &request_url,
        attempt: &second_attempt,
    }))
    .expect("select revised account");
    block_on(selector.record_failure(
        &second,
        CodexAccountFailure::CloudflareChallenge { retry_after: None },
    ))
    .expect("record challenge");

    let account = store.account("acct_primary").expect("account");
    let cooldown_until = account.cooldown_until().expect("cooldown");
    let data = block_on(store.repository().load_complete_data(&account)).expect("credential data");
    assert_eq!(data.cookies.len(), 1);
    assert!(
        data.cookies[0]
            .expires_at
            .is_some_and(|expires_at| SystemTime::from(expires_at) <= cooldown_until)
    );
}

#[test]
fn cloudflare_path_block_deletes_provider_owned_cookies() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let required = ProviderAccountId::new("acct_primary").expect("account id");
    let request_url =
        Url::parse("https://chatgpt.com/backend-api/codex/responses").expect("request URL");
    let first_attempt = attempt_with_required(BTreeSet::new(), Some(required.clone()));
    let first = block_on(selector.select(&SelectCodexCredential {
        provider_instance_id: &instance_id(),
        upstream_model: "gpt-5.4",
        request_url: &request_url,
        attempt: &first_attempt,
    }))
    .expect("select account");
    block_on(selector.capture_response_cookies(
        &first,
        &request_url,
        &["__cf_bm=old; Path=/; Domain=chatgpt.com; Secure; Max-Age=3600".to_owned()],
    ))
    .expect("capture cookie");

    let second_attempt = attempt_with_required(BTreeSet::new(), Some(required));
    let second = block_on(selector.select(&SelectCodexCredential {
        provider_instance_id: &instance_id(),
        upstream_model: "gpt-5.4",
        request_url: &request_url,
        attempt: &second_attempt,
    }))
    .expect("select revised account");
    block_on(selector.record_failure(&second, CodexAccountFailure::CloudflarePathBlocked))
        .expect("record path block");

    let account = store.account("acct_primary").expect("account");
    let data = block_on(store.repository().load_complete_data(&account)).expect("credential data");
    assert!(data.cookies.is_empty());
}
