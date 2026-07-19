use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::executor::block_on;
use gateway_core::engine::credential::{
    AccountAvailability, AccountSelectionPolicy, ProviderAccountId, RotationStrategy,
};
use gateway_core::engine::{
    AccountSelectionConstraints, AttemptContext, CancellationToken, ModelRequestId,
    UpstreamSendState,
};
use gateway_core::error::ProviderErrorKind;
use provider_openai::credential::{
    CodexCookiePolicy, CodexCredentialCodec, CodexCredentialSelector, CreateCodexCredential,
    CredentialSelectionError, SelectCodexCredential,
};
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
        AccountSelectionConstraints::new(excluded_accounts, required_account),
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
        AccountSelectionConstraints::new(BTreeSet::new(), None),
        None,
        CancellationToken::new(),
    )
}

fn selector(
    store: &Arc<MemoryAccountStore>,
    leases: Arc<TestLeaseCoordinator>,
) -> CodexCredentialSelector {
    CodexCredentialSelector::new(
        store.repository(),
        leases,
        CodexCookiePolicy::official().expect("official cookie policy"),
    )
}

#[test]
fn codec_persists_tokens_as_plaintext_provider_json() {
    let encoded = CodexCredentialCodec::encode(&secret("literal-access-token"), Vec::new())
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
        ["access_token", "cookies", "refresh_token", "schema_version"]
    );
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
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
            }),
        )
        .expect("select account");

    assert_eq!(lease.account_id().as_str(), "acct_primary");
    let requests = leases.requests.lock().expect("lease requests lock");
    assert_eq!(requests[0].max_concurrent, 2);
    assert_eq!(requests[0].request_interval, Duration::from_millis(10));
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
fn unauthorized_failure_marks_unified_account_invalid() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let attempt = attempt(BTreeSet::new());
    let lease =
        block_on(
            selector.select(&SelectCodexCredential {
                provider_instance_id: &instance_id(),
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
            }),
        )
        .expect("select account");
    block_on(selector.record_failure(
        &lease,
        ProviderErrorKind::Unauthorized,
        UpstreamSendState::Sent,
        None,
    ));
    assert_eq!(
        store
            .account("acct_primary")
            .expect("account")
            .availability(),
        AccountAvailability::Invalid
    );
}

#[test]
fn permission_denied_failure_keeps_every_account_ready() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    create_account(&store, "acct_other", "at-other");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let attempt = attempt(BTreeSet::new());
    let lease =
        block_on(
            selector.select(&SelectCodexCredential {
                provider_instance_id: &instance_id(),
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
            }),
        )
        .expect("select account");

    block_on(selector.record_failure(
        &lease,
        ProviderErrorKind::PermissionDenied,
        UpstreamSendState::Sent,
        None,
    ));

    assert_eq!(
        (
            store
                .account("acct_primary")
                .expect("primary account")
                .availability(),
            store
                .account("acct_other")
                .expect("other account")
                .availability(),
        ),
        (AccountAvailability::Ready, AccountAvailability::Ready)
    );
}
