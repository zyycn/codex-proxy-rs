use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::executor::block_on;
use gateway_core::engine::continuation::{
    ContinuationBinding, NativeContinuationPin, PreviousResponseId,
};
use gateway_core::engine::credential::{
    AccountAttemptFeedback, AccountConcurrencyLimit, AccountErrorReason, AccountFeedbackStats,
    AccountSelectionPolicy, AccountStateChange, AccountWeight, CredentialState, OpaqueProviderData,
    ProviderAccount, ProviderAccountId, ProviderAccountStore as _, QuotaAccessChange,
    QuotaAccessState, QuotaEvidence, QuotaObservation, QuotaState, QuotaWriteOutcome,
    RotationStrategy,
};
use gateway_core::engine::{
    AccountAttemptContext, AttemptContext, CancellationToken, ModelRequestId, RequestAttemptContext,
};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::provider_ports::{
    ProviderCooldownPort, ProviderSessionAffinityKey, ProviderSessionAffinityPort,
};
use gateway_core::routing::{
    ClientRoutingScope, FrozenAccountScope, ProviderKind, RuntimeAccount, RuntimeAccountDirectory,
};
use provider_openai::OFFICIAL_CODEX_BASE_URL;
use provider_openai::credential::{
    CodexAccountFailure, CodexCookiePolicy, CodexCredentialCatalogService, CodexCredentialCodec,
    CodexCredentialQuotaService, CodexCredentialSelector, CredentialSelectionError,
    ImportCodexOAuthCredential, SelectCodexCredential,
};
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use secrecy::ExposeSecret;
use serde_json::json;
use url::Url;

use crate::support::{
    MemoryAccountStore, MemoryCooldownPort, MemorySessionAffinity, MemorySessionExclusions,
    TestLeaseCoordinator, account_policy, catalog_cache, profile, secret,
};

fn create_account(store: &Arc<MemoryAccountStore>, id: &str, token: &str) {
    block_on(store.seed_oauth_credential(ImportCodexOAuthCredential {
        account_id: id.to_owned(),
        name: id.to_owned(),
        secret: secret(token),
        verified_account: profile(&format!("chatgpt-{id}")),
        next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
        enabled: true,
    }));
}

fn contract_account_scope() -> Arc<FrozenAccountScope> {
    let provider = ProviderKind::new("openai").expect("provider");
    let accounts = [
        "acct_available",
        "acct_fallback",
        "acct_fallback_signal",
        "acct_first",
        "acct_missing",
        "acct_original",
        "acct_original_signal",
        "acct_other",
        "acct_primary",
        "acct_second",
    ]
    .into_iter()
    .map(|id| {
        (
            ProviderAccountId::new(id).expect("account"),
            RuntimeAccount::new(provider.clone(), BTreeSet::new()),
        )
    })
    .collect::<BTreeMap<_, _>>();
    Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(accounts)),
        ClientRoutingScope::all_accounts(),
    ))
}

fn attempt(excluded_accounts: BTreeSet<ProviderAccountId>) -> AttemptContext {
    attempt_with_required(excluded_accounts, None)
}

fn attempt_with_required(
    excluded_accounts: BTreeSet<ProviderAccountId>,
    required_account: Option<ProviderAccountId>,
) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_codex_contract").expect("request id"),
            ClientApiKeyId::new("key_codex_contract").expect("client key id"),
        ),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(excluded_accounts, required_account, None)
            .with_account_scope(contract_account_scope()),
        None,
        CancellationToken::new(),
    )
}

fn round_robin_attempt() -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_codex_round_robin").expect("request id"),
            ClientApiKeyId::new("key_codex_contract").expect("client key id"),
        ),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        AccountSelectionPolicy::new(
            RotationStrategy::RoundRobin,
            NonZeroU32::new(2).expect("concurrency"),
            Duration::ZERO,
        ),
        AccountAttemptContext::new(BTreeSet::new(), None, None)
            .with_account_scope(contract_account_scope()),
        None,
        CancellationToken::new(),
    )
}

fn selector(
    store: &Arc<MemoryAccountStore>,
    leases: Arc<TestLeaseCoordinator>,
) -> CodexCredentialSelector {
    selector_with_affinity(store, leases, Arc::new(MemorySessionAffinity::default()))
}

fn selector_with_affinity(
    store: &Arc<MemoryAccountStore>,
    leases: Arc<TestLeaseCoordinator>,
    session_affinity: Arc<MemorySessionAffinity>,
) -> CodexCredentialSelector {
    selector_with_runtime(
        store,
        leases,
        session_affinity,
        Arc::new(AccountFeedbackStats::default()),
        Arc::new(MemoryCooldownPort::new()),
    )
}

fn selector_with_runtime(
    store: &Arc<MemoryAccountStore>,
    leases: Arc<TestLeaseCoordinator>,
    session_affinity: Arc<MemorySessionAffinity>,
    account_feedback: Arc<AccountFeedbackStats>,
    cooldowns: Arc<dyn ProviderCooldownPort>,
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
        verified_at: chrono::Utc::now(),
    });
    let http = reqwest::Client::builder().build().expect("HTTP client");
    let catalog = Arc::new(CodexCredentialCatalogService::new(
        store.repository(),
        profile.clone(),
        http.clone(),
        OFFICIAL_CODEX_BASE_URL.to_owned(),
        catalog_cache(),
    ));
    let quota = Arc::new(CodexCredentialQuotaService::new(
        store.repository(),
        profile,
        http,
        OFFICIAL_CODEX_BASE_URL.to_owned(),
        cooldowns,
    ));
    CodexCredentialSelector::new(
        ProviderKind::new("openai").expect("provider"),
        store.repository(),
        leases,
        session_affinity,
        Arc::new(MemorySessionExclusions::default()),
        catalog,
        quota,
        account_feedback,
        CodexCookiePolicy::official().expect("official cookie policy"),
    )
}

fn persist_credential_state(
    store: &MemoryAccountStore,
    account: &ProviderAccount,
    credential_state: CredentialState,
) {
    block_on(store.apply_state_change(AccountStateChange {
        account_id: account.id().clone(),
        expected_revision: account.revision(),
        credential_state,
        observed_at: SystemTime::now(),
        error_reason: credential_state.error_reason(),
        message: None,
    }))
    .expect("persist credential state");
}

fn persist_quota_exhaustion(
    store: &MemoryAccountStore,
    account: &ProviderAccount,
    reset_at: Option<SystemTime>,
) {
    block_on(store.apply_quota_access(QuotaAccessChange {
        account_id: account.id().clone(),
        expected_revision: account.revision(),
        state: QuotaState::exhausted(
            QuotaEvidence::UsageLimitReached,
            SystemTime::now(),
            reset_at,
        ),
    }))
    .expect("persist quota exhaustion");
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
        .installation_id()
        .to_owned();

    let preserved = CodexCredentialCodec::preserve_installation_id(&incoming, &existing)
        .expect("preserve installation ID");
    let preserved = CodexCredentialCodec::decode_complete(&preserved).expect("preserved data");

    assert_eq!(preserved.installation_id(), existing_id);
    assert_eq!(
        preserved.oauth().expect("OAuth data").access_token,
        "incoming-access-token"
    );
}

#[test]
fn codec_reimport_preserves_installation_id_without_principal_validation() {
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

    let existing_id = CodexCredentialCodec::decode_complete(&existing)
        .expect("existing data")
        .installation_id()
        .to_owned();
    let preserved = CodexCredentialCodec::preserve_installation_id(&incoming, &existing)
        .expect("preserve installation ID");
    let preserved = CodexCredentialCodec::decode_complete(&preserved).expect("preserved data");

    assert_eq!(preserved.installation_id(), existing_id);
    assert_eq!(
        preserved.oauth().expect("OAuth data").access_token,
        "incoming-access-token"
    );
}

#[test]
fn repository_round_trips_plaintext_runtime_secret() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let account = store.account("acct_primary").expect("account");
    let runtime = block_on(store.repository().load_runtime_credential(&account))
        .expect("load runtime credential");
    let oauth = runtime.authentication.oauth().expect("OAuth credential");
    assert_eq!(oauth.access_token.expose_secret(), "at-primary");
    assert_eq!(
        oauth
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
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
                session_affinity_key: None,
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
    assert_eq!(
        requests[0].provider_kind(),
        &ProviderKind::new("openai").expect("provider")
    );
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
fn selector_uses_the_account_concurrency_override_for_the_redis_lease() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    store.set_scheduling(
        "acct_primary",
        Some(AccountConcurrencyLimit::new(7).expect("concurrency override")),
        AccountWeight::DEFAULT,
    );
    let leases = Arc::new(TestLeaseCoordinator::default());
    let selector = selector(&store, Arc::clone(&leases));
    let attempt = attempt(BTreeSet::new());

    block_on(selector.select(&SelectCodexCredential {
        upstream_model: "gpt-5.4",
        request_url:
            &Url::parse("https://chatgpt.com/backend-api/codex/responses").expect("request URL"),
        attempt: &attempt,
        session_affinity_key: None,
    }))
    .expect("select account");

    let requests = leases.requests.lock().expect("lease requests lock");
    assert_eq!(requests[0].max_concurrent().get(), 7);
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
            upstream_model: "gpt-5.4",
            request_url: &request_url,
            attempt: &attempt,
            session_affinity_key: None,
        }))
        .expect("select round robin account");
        selected.push(lease.account_id().as_str().to_owned());
    }

    assert_eq!(
        selected,
        ["acct_first", "acct_second", "acct_first", "acct_second"]
    );
}

#[tokio::test]
async fn selector_should_claim_the_initial_session_account_before_upstream_send() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "at-first");
    create_account(&store, "acct_second", "at-second");
    let affinity = Arc::new(MemorySessionAffinity::default());
    let selector = selector_with_affinity(
        &store,
        Arc::new(TestLeaseCoordinator::default()),
        Arc::clone(&affinity),
    );
    let provider = ProviderKind::new("openai").expect("provider");
    let key = ProviderSessionAffinityKey::try_new("initial-claim").expect("affinity key");
    let request_attempt = attempt(BTreeSet::new());

    let selected = selector
        .select(&SelectCodexCredential {
            upstream_model: "gpt-5.4",
            request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                .expect("request URL"),
            attempt: &request_attempt,
            session_affinity_key: Some(&key),
        })
        .await
        .expect("select initial account");

    assert_eq!(
        affinity
            .load(&provider, &key)
            .await
            .expect("load claimed affinity"),
        Some(selected.account_id().clone())
    );
}

#[tokio::test]
async fn record_success_should_not_overwrite_a_newer_session_winner() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "at-first");
    create_account(&store, "acct_second", "at-second");
    let affinity = Arc::new(MemorySessionAffinity::default());
    let selector = selector_with_affinity(
        &store,
        Arc::new(TestLeaseCoordinator::default()),
        Arc::clone(&affinity),
    );
    let provider = ProviderKind::new("openai").expect("provider");
    let key = ProviderSessionAffinityKey::try_new("late-success").expect("affinity key");
    let first = store.account("acct_first").expect("first account");
    let second = ProviderAccountId::new("acct_second").expect("second account");
    affinity
        .bind(&provider, &key, &second, Duration::from_secs(60))
        .await
        .expect("seed newer affinity");

    selector
        .record_success(&first, Some(&key), first.id())
        .await;

    assert_eq!(
        affinity
            .load(&provider, &key)
            .await
            .expect("load preserved affinity"),
        Some(second)
    );
}

#[tokio::test]
async fn selector_should_reuse_the_account_bound_to_the_same_session() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "at-first");
    create_account(&store, "acct_second", "at-second");
    let affinity = Arc::new(MemorySessionAffinity::default());
    let selector = selector_with_affinity(
        &store,
        Arc::new(TestLeaseCoordinator::default()),
        Arc::clone(&affinity),
    );
    let key = ProviderSessionAffinityKey::try_new("same-session").expect("affinity key");
    let request_url =
        Url::parse("https://chatgpt.com/backend-api/codex/responses").expect("request URL");
    let first_attempt = attempt(BTreeSet::new());
    let first = selector
        .select(&SelectCodexCredential {
            upstream_model: "gpt-5.4",
            request_url: &request_url,
            attempt: &first_attempt,
            session_affinity_key: Some(&key),
        })
        .await
        .expect("select first account");
    selector
        .record_success(first.account(), Some(&key), first.account_id())
        .await;
    let first_account = first.account_id().clone();

    let second_attempt = attempt(BTreeSet::new());
    let second = selector
        .select(&SelectCodexCredential {
            upstream_model: "gpt-5.4",
            request_url: &request_url,
            attempt: &second_attempt,
            session_affinity_key: Some(&key),
        })
        .await
        .expect("select bound account");

    assert_eq!(
        (
            second.account_id().as_str(),
            second.affinity_hit(),
            second.escape_reason(),
            second.account_switch(),
        ),
        (first_account.as_str(), true, None, false)
    );
    assert_eq!(
        affinity
            .load(&ProviderKind::new("openai").expect("provider"), &key)
            .await
            .expect("load affinity"),
        Some(first_account)
    );
}

#[tokio::test]
async fn selector_should_replace_a_busy_affinity_binding_after_the_fallback_succeeds() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "at-first");
    create_account(&store, "acct_second", "at-second");
    let leases = Arc::new(TestLeaseCoordinator::default());
    leases
        .busy_accounts
        .lock()
        .expect("busy account lock")
        .insert(ProviderAccountId::new("acct_first").expect("account"));
    let affinity = Arc::new(MemorySessionAffinity::default());
    let provider = ProviderKind::new("openai").expect("provider");
    let key = ProviderSessionAffinityKey::try_new("busy-session").expect("affinity key");
    let bound = ProviderAccountId::new("acct_first").expect("bound account");
    affinity
        .bind(&provider, &key, &bound, Duration::from_secs(60))
        .await
        .expect("seed affinity");
    let selector = selector_with_affinity(&store, leases, Arc::clone(&affinity));
    let request_url =
        Url::parse("https://chatgpt.com/backend-api/codex/responses").expect("request URL");
    let request_attempt = attempt(BTreeSet::new());

    let selected = selector
        .select(&SelectCodexCredential {
            upstream_model: "gpt-5.4",
            request_url: &request_url,
            attempt: &request_attempt,
            session_affinity_key: Some(&key),
        })
        .await
        .expect("select fallback account");
    selector
        .record_success(selected.account(), Some(&key), &bound)
        .await;

    assert_eq!(
        (
            selected.account_id().as_str(),
            selected.affinity_hit(),
            selected.escape_reason(),
            selected.account_switch(),
        ),
        ("acct_second", false, Some("lease_saturated"), true)
    );
    assert_eq!(
        affinity
            .load(&provider, &key)
            .await
            .expect("load replaced affinity"),
        Some(ProviderAccountId::new("acct_second").expect("second account"))
    );
}

#[tokio::test]
async fn selector_should_keep_a_schedulable_affinity_account_despite_soft_health_signals() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "at-first");
    create_account(&store, "acct_second", "at-second");
    let affinity = Arc::new(MemorySessionAffinity::default());
    let provider = ProviderKind::new("openai").expect("provider");
    let key = ProviderSessionAffinityKey::try_new("unhealthy-session").expect("affinity key");
    let bound = ProviderAccountId::new("acct_first").expect("bound account");
    affinity
        .bind(&provider, &key, &bound, Duration::from_secs(60))
        .await
        .expect("seed affinity");
    let account_feedback = Arc::new(AccountFeedbackStats::default());
    let selector = selector_with_runtime(
        &store,
        Arc::new(TestLeaseCoordinator::default()),
        affinity,
        Arc::clone(&account_feedback),
        Arc::new(MemoryCooldownPort::new()),
    );
    for _ in 0..4 {
        account_feedback.report(
            &provider,
            &bound,
            AccountAttemptFeedback::Failed {
                first_output_ms: None,
            },
        );
    }
    let request_url =
        Url::parse("https://chatgpt.com/backend-api/codex/responses").expect("request URL");
    let request_attempt = attempt(BTreeSet::new());

    let selected = selector
        .select(&SelectCodexCredential {
            upstream_model: "gpt-5.4",
            request_url: &request_url,
            attempt: &request_attempt,
            session_affinity_key: Some(&key),
        })
        .await
        .expect("select bound account");

    assert_eq!(
        (
            selected.account_id().as_str(),
            selected.affinity_hit(),
            selected.escape_reason(),
            selected.account_switch(),
        ),
        ("acct_first", true, None, false)
    );
}

#[tokio::test]
async fn selector_should_escape_a_quota_exhausted_affinity_account() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "at-first");
    create_account(&store, "acct_second", "at-second");
    let first = store.account("acct_first").expect("first account");
    persist_quota_exhaustion(&store, &first, None);
    let affinity = Arc::new(MemorySessionAffinity::default());
    let provider = ProviderKind::new("openai").expect("provider");
    let key = ProviderSessionAffinityKey::try_new("quota-session").expect("affinity key");
    affinity
        .bind(&provider, &key, first.id(), Duration::from_secs(60))
        .await
        .expect("seed affinity");
    let selector =
        selector_with_affinity(&store, Arc::new(TestLeaseCoordinator::default()), affinity);
    let request_url =
        Url::parse("https://chatgpt.com/backend-api/codex/responses").expect("request URL");
    let request_attempt = attempt(BTreeSet::new());

    let selected = selector
        .select(&SelectCodexCredential {
            upstream_model: "gpt-5.4",
            request_url: &request_url,
            attempt: &request_attempt,
            session_affinity_key: Some(&key),
        })
        .await
        .expect("select fallback account");

    assert_eq!(
        (
            selected.account_id().as_str(),
            selected.affinity_hit(),
            selected.escape_reason(),
            selected.account_switch(),
        ),
        ("acct_second", false, Some("quota_exhausted"), true)
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
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
                session_affinity_key: None,
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
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
                session_affinity_key: None,
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
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
                session_affinity_key: None,
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
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
                session_affinity_key: None,
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
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
                session_affinity_key: None,
            }),
        )
        .expect("select account");
    block_on(selector.record_failure(
        lease.account(),
        CodexAccountFailure::CredentialExpired,
        None,
    ))
    .expect("record credential expiry");
    assert_eq!(
        store
            .account("acct_primary")
            .expect("account")
            .credential_state(),
        CredentialState::Expired
    );
    let account = store.account("acct_primary").expect("expired account");
    assert_eq!(
        account.last_error_reason(),
        Some(AccountErrorReason::AccessTokenExpired)
    );
    assert_eq!(account.last_error_message(), None);
}

#[test]
fn credential_expired_failure_keeps_expired_oauth_for_bounded_refresh_recovery() {
    let store = Arc::new(MemoryAccountStore::default());
    let mut expired_profile = profile("chatgpt-acct_primary");
    expired_profile.access_token_expires_at =
        Some(chrono::Utc::now() - chrono::Duration::minutes(1));
    block_on(store.seed_oauth_credential(ImportCodexOAuthCredential {
        account_id: "acct_primary".to_owned(),
        name: "acct_primary".to_owned(),
        secret: secret("at-primary"),
        verified_account: expired_profile,
        next_refresh_at: Some(chrono::Utc::now() + chrono::Duration::minutes(10)),
        enabled: true,
    }));
    let account = store.account("acct_primary").expect("account");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));

    block_on(selector.record_failure(
        &account,
        CodexAccountFailure::CredentialExpired,
        Some("token_expired".to_owned()),
    ))
    .expect("record credential expiry");

    let retained = store
        .account("acct_primary")
        .expect("account retained for refresh");
    assert_eq!(retained.credential_state(), CredentialState::Ready);
    assert_eq!(
        retained.last_error_reason(),
        Some(AccountErrorReason::AccessTokenExpired)
    );
    assert_eq!(retained.last_error_message(), Some("token_expired"));
}

#[test]
fn rate_limited_failure_records_runtime_cooldown_without_changing_persisted_facts() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let cooldowns = Arc::new(MemoryCooldownPort::new());
    let selector = selector_with_runtime(
        &store,
        Arc::new(TestLeaseCoordinator::default()),
        Arc::new(MemorySessionAffinity::default()),
        Arc::new(AccountFeedbackStats::default()),
        Arc::clone(&cooldowns) as Arc<dyn ProviderCooldownPort>,
    );
    let attempt = attempt(BTreeSet::new());
    let lease =
        block_on(
            selector.select(&SelectCodexCredential {
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
                session_affinity_key: None,
            }),
        )
        .expect("select account");

    block_on(selector.record_failure(
        lease.account(),
        CodexAccountFailure::RateLimited {
            retry_after: Some(Duration::from_secs(30)),
        },
        None,
    ))
    .expect("record rate-limit failure");

    let account = store.account("acct_primary").expect("account");
    assert_eq!(account.credential_state(), CredentialState::Ready);
    assert_eq!(account.quota().access(), QuotaAccessState::Unknown);
    assert!(
        store.quota_json("acct_primary").is_none(),
        "429 must not synthesize quota window"
    );
    let cooldown = block_on(cooldowns.read(account.id())).expect("read cooldown");
    assert!(
        cooldown.is_some_and(|cooling| cooling.until() > SystemTime::now()),
        "429 must record a Redis cooldown expiring in the future"
    );
}

#[tokio::test]
async fn usage_limit_exhaustion_marks_quota_exhausted_without_usage_probe() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let account = store.account("acct_primary").expect("account");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));

    selector
        .record_failure(
            &account,
            CodexAccountFailure::UsageLimitExhausted {
                reset_at: Some(SystemTime::now() + Duration::from_secs(30)),
            },
            None,
        )
        .await
        .expect("record usage-limit exhaustion");

    let account = store.account("acct_primary").expect("persisted account");
    assert_eq!(account.quota().access(), QuotaAccessState::Exhausted);
    assert_eq!(
        account.quota().evidence(),
        Some(QuotaEvidence::UsageLimitReached)
    );
    assert_eq!(store.quota_reads(), 0);
}

#[test]
fn rate_limited_failure_does_not_downgrade_persisted_quota_exhaustion() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let account = store.account("acct_primary").expect("account");
    persist_quota_exhaustion(&store, &account, None);
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));

    block_on(selector.record_failure(
        &account,
        CodexAccountFailure::RateLimited {
            retry_after: Some(Duration::from_secs(30)),
        },
        None,
    ))
    .expect("record rate-limit failure");

    assert_eq!(
        store
            .account("acct_primary")
            .expect("persisted account")
            .quota()
            .access(),
        QuotaAccessState::Exhausted
    );
}

#[test]
fn rate_limited_failure_does_not_consult_stale_quota_snapshot() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let account = store.account("acct_primary").expect("account");
    let cooldowns = Arc::new(MemoryCooldownPort::new());
    let selector = selector_with_runtime(
        &store,
        Arc::new(TestLeaseCoordinator::default()),
        Arc::new(MemorySessionAffinity::default()),
        Arc::new(AccountFeedbackStats::default()),
        Arc::clone(&cooldowns) as Arc<dyn ProviderCooldownPort>,
    );

    block_on(selector.record_failure(
        &account,
        CodexAccountFailure::RateLimited {
            retry_after: Some(Duration::from_secs(30)),
        },
        None,
    ))
    .expect("record rate-limit failure");

    assert_eq!(
        store
            .account("acct_primary")
            .expect("persisted account")
            .credential_state(),
        CredentialState::Ready
    );
    // 429 临时限流写入 Redis 冷却，不读写 quota JSON。
    assert_eq!(store.quota_reads(), 0);
    let cooldown = block_on(cooldowns.read(account.id())).expect("read cooldown");
    assert!(
        cooldown.is_some_and(|cooling| cooling.until() > SystemTime::now()),
        "429 must record a Redis cooldown"
    );
}

#[test]
fn rate_limited_failure_does_not_overwrite_stale_authentication_state() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let account = store.account("acct_primary").expect("account");
    persist_credential_state(&store, &account, CredentialState::Invalid);
    let current = store.account("acct_primary").expect("invalid account");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));

    block_on(selector.record_failure(
        &current,
        CodexAccountFailure::RateLimited {
            retry_after: Some(Duration::from_secs(30)),
        },
        None,
    ))
    .expect("record rate-limit failure");

    assert_eq!(
        store
            .account("acct_primary")
            .expect("persisted account")
            .credential_state(),
        CredentialState::Invalid
    );
}

#[test]
fn successful_upstream_response_recovers_non_quota_terminal_states() {
    for stale in [
        CredentialState::Expired,
        CredentialState::Invalid,
        CredentialState::Banned,
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_primary", "at-primary");
        let account = store.account("acct_primary").expect("account");
        persist_credential_state(&store, &account, stale);
        let current = store.account("acct_primary").expect("stale account");
        let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));

        block_on(selector.record_success(&current, None, current.id()));

        assert_eq!(
            store
                .account("acct_primary")
                .expect("recovered account")
                .credential_state(),
            CredentialState::Ready,
            "stale state {stale:?}",
        );
    }
}

#[test]
fn elapsed_quota_reset_remains_blocked_until_authoritative_recovery() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let account = store.account("acct_primary").expect("account");
    persist_quota_exhaustion(
        &store,
        &account,
        Some(SystemTime::now() - Duration::from_secs(1)),
    );
    let blocked = store.account("acct_primary").expect("blocked account");
    assert!(blocked.quota().is_exhausted());
    assert_eq!(
        blocked
            .status_projection(SystemTime::now(), None)
            .status
            .as_str(),
        "quota_exhausted"
    );
}

#[test]
fn rate_limited_failures_for_distinct_accounts_do_not_conflict() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "at-first");
    create_account(&store, "acct_second", "at-second");
    let first = store.account("acct_first").expect("first account");
    let second = store.account("acct_second").expect("second account");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));

    block_on(async {
        let (first_result, second_result) = futures::join!(
            selector.record_failure(
                &first,
                CodexAccountFailure::RateLimited {
                    retry_after: Some(Duration::from_secs(30)),
                },
                None,
            ),
            selector.record_failure(
                &second,
                CodexAccountFailure::RateLimited {
                    retry_after: Some(Duration::from_secs(30)),
                },
                None,
            ),
        );
        first_result.expect("record first account failure");
        second_result.expect("record second account failure");
    });

    assert_eq!(
        [
            store
                .account("acct_first")
                .expect("persisted first account")
                .credential_state(),
            store
                .account("acct_second")
                .expect("persisted second account")
                .credential_state(),
        ],
        [CredentialState::Ready; 2]
    );
}

#[test]
fn native_continuation_surfaces_the_original_accounts_quota_status_to_the_coordinator() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_original", "at-original");
    create_account(&store, "acct_fallback", "at-fallback");
    let strict_selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let original = store.account("acct_original").expect("original account");
    block_on(strict_selector.record_failure(&original, CodexAccountFailure::QuotaExhausted, None))
        .expect("mark original account exhausted");

    let selector = selector_with_runtime(
        &store,
        Arc::new(TestLeaseCoordinator::default()),
        Arc::new(MemorySessionAffinity::default()),
        Arc::new(AccountFeedbackStats::default()),
        Arc::new(MemoryCooldownPort::new()),
    );
    let continuation = NativeContinuationPin::new(
        PreviousResponseId::new("previous-response"),
        PreviousResponseId::new("upstream-response"),
        ClientApiKeyId::new("key_codex_contract").expect("client key id"),
        ProviderKind::new("openai").expect("provider"),
        original.id().clone(),
    );
    let attempt = AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_native_continuation").expect("request id"),
            ClientApiKeyId::new("key_codex_contract").expect("client key id"),
        ),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None)
            .with_account_scope(contract_account_scope()),
        Some(ContinuationBinding::Pinned(continuation)),
        CancellationToken::new(),
    );
    let error =
        block_on(
            selector.select(&SelectCodexCredential {
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
                session_affinity_key: None,
            }),
        )
        .expect_err("the selector must surface the unavailable native account");

    assert!(matches!(
        error,
        CredentialSelectionError::NoEligibleCredential
    ));
}

#[test]
fn native_continuation_surfaces_the_original_accounts_quota_signal_to_the_coordinator() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_original_signal", "at-original-signal");
    create_account(&store, "acct_fallback_signal", "at-fallback-signal");
    let original = store
        .account("acct_original_signal")
        .expect("original account");
    let quota = json!({
        "rate_limit": {
            "allowed": false,
            "limit_reached": true,
            "primary_window": {"used_percent": 98}
        }
    });
    let observed_at = SystemTime::now();
    let outcome = block_on(store.compare_and_swap_quota(QuotaObservation {
        account_id: original.id().clone(),
        expected_revision: original.revision(),
        quota: OpaqueProviderData::new(quota.as_object().expect("quota object").clone()),
        observed_at,
        state: QuotaState::exhausted(QuotaEvidence::ProviderDenied, observed_at, None),
    }))
    .expect("persist quota signal");
    assert!(matches!(outcome, QuotaWriteOutcome::Updated));

    let selector = selector_with_runtime(
        &store,
        Arc::new(TestLeaseCoordinator::default()),
        Arc::new(MemorySessionAffinity::default()),
        Arc::new(AccountFeedbackStats::default()),
        Arc::new(MemoryCooldownPort::new()),
    );
    let continuation = NativeContinuationPin::new(
        PreviousResponseId::new("previous-response"),
        PreviousResponseId::new("upstream-response"),
        ClientApiKeyId::new("key_codex_contract").expect("client key id"),
        ProviderKind::new("openai").expect("provider"),
        original.id().clone(),
    );
    let attempt = AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_native_continuation_signal").expect("request id"),
            ClientApiKeyId::new("key_codex_contract").expect("client key id"),
        ),
        NonZeroU32::new(1).expect("attempt"),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None)
            .with_account_scope(contract_account_scope()),
        Some(ContinuationBinding::Pinned(continuation)),
        CancellationToken::new(),
    );
    let error =
        block_on(
            selector.select(&SelectCodexCredential {
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
                session_affinity_key: None,
            }),
        )
        .expect_err("the selector must surface the quota-limited native account");

    assert!(matches!(
        error,
        CredentialSelectionError::NoEligibleCredential
    ));
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
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
                session_affinity_key: None,
            }),
        )
        .expect("select account");

    block_on(selector.record_failure(
        lease.account(),
        CodexAccountFailure::IdentityVerificationRequired,
        None,
    ))
    .expect("record identity verification failure");

    assert_eq!(
        store
            .account(lease.account_id().as_str())
            .expect("selected account")
            .credential_state(),
        CredentialState::Invalid
    );
    let other = if lease.account_id().as_str() == "acct_primary" {
        "acct_other"
    } else {
        "acct_primary"
    };
    assert_eq!(
        store
            .account(other)
            .expect("other account")
            .credential_state(),
        CredentialState::Ready
    );
}

#[test]
fn cloudflare_challenge_does_not_change_persisted_account_facts() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let attempt = attempt(BTreeSet::new());
    let lease =
        block_on(
            selector.select(&SelectCodexCredential {
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
                session_affinity_key: None,
            }),
        )
        .expect("select account");

    block_on(selector.record_failure(
        lease.account(),
        CodexAccountFailure::CloudflareChallenge { retry_after: None },
        None,
    ))
    .expect("record challenge");

    assert_eq!(
        store
            .account("acct_primary")
            .expect("account")
            .credential_state(),
        CredentialState::Ready
    );
    block_on(selector.record_success(lease.account(), None, lease.account_id()));
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
                upstream_model: "gpt-5.4",
                request_url: &Url::parse("https://chatgpt.com/backend-api/codex/responses")
                    .expect("request URL"),
                attempt: &attempt,
                session_affinity_key: None,
            }),
        )
        .expect("select account");

    for _ in 0..3 {
        block_on(selector.record_failure(
            lease.account(),
            CodexAccountFailure::CloudflarePathBlocked,
            None,
        ))
        .expect("record path block");
    }

    assert_eq!(
        store
            .account("acct_primary")
            .expect("affected account")
            .credential_state(),
        CredentialState::Invalid
    );
    assert_eq!(
        store
            .account("acct_other")
            .expect("other account")
            .credential_state(),
        CredentialState::Ready
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
        upstream_model: "gpt-5.4",
        request_url: &request_url,
        attempt: &first_attempt,
        session_affinity_key: None,
    }))
    .expect("select account");
    block_on(selector.capture_response_cookies(
        first.account(),
        &request_url,
        &["cf_clearance=old; Path=/; Domain=chatgpt.com; Secure; Max-Age=3600".to_owned()],
    ))
    .expect("capture cookie");

    let second_attempt = attempt_with_required(BTreeSet::new(), Some(required));
    let second = block_on(selector.select(&SelectCodexCredential {
        upstream_model: "gpt-5.4",
        request_url: &request_url,
        attempt: &second_attempt,
        session_affinity_key: None,
    }))
    .expect("select revised account");
    block_on(selector.record_failure(
        second.account(),
        CodexAccountFailure::CloudflareChallenge { retry_after: None },
        None,
    ))
    .expect("record challenge");

    let account = store.account("acct_primary").expect("account");
    let data = block_on(store.repository().load_complete_data(&account)).expect("credential data");
    assert_eq!(data.cookies().len(), 1);
    assert!(data.cookies()[0].expires_at.is_some_and(|expires_at| {
        let expires_at = SystemTime::from(expires_at);
        expires_at > SystemTime::now() && expires_at <= SystemTime::now() + Duration::from_secs(120)
    }));
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
        upstream_model: "gpt-5.4",
        request_url: &request_url,
        attempt: &first_attempt,
        session_affinity_key: None,
    }))
    .expect("select account");
    block_on(selector.capture_response_cookies(
        first.account(),
        &request_url,
        &["__cf_bm=old; Path=/; Domain=chatgpt.com; Secure; Max-Age=3600".to_owned()],
    ))
    .expect("capture cookie");

    let second_attempt = attempt_with_required(BTreeSet::new(), Some(required));
    let second = block_on(selector.select(&SelectCodexCredential {
        upstream_model: "gpt-5.4",
        request_url: &request_url,
        attempt: &second_attempt,
        session_affinity_key: None,
    }))
    .expect("select revised account");
    block_on(selector.record_failure(
        second.account(),
        CodexAccountFailure::CloudflarePathBlocked,
        None,
    ))
    .expect("record path block");

    let account = store.account("acct_primary").expect("account");
    let data = block_on(store.repository().load_complete_data(&account)).expect("credential data");
    assert!(data.cookies().is_empty());
}

#[test]
fn response_cookie_rotation_returns_a_current_account_for_later_fenced_writes() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_primary", "at-primary");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let request_url =
        Url::parse("https://chatgpt.com/backend-api/codex/responses").expect("request URL");
    let attempt = attempt_with_required(
        BTreeSet::new(),
        Some(ProviderAccountId::new("acct_primary").expect("account id")),
    );
    let lease = block_on(selector.select(&SelectCodexCredential {
        upstream_model: "gpt-5.4",
        request_url: &request_url,
        attempt: &attempt,
        session_affinity_key: None,
    }))
    .expect("select account");

    let outcome = block_on(selector.capture_response_cookies(
        lease.account(),
        &request_url,
        &["cf_clearance=updated; Path=/; Domain=chatgpt.com; Secure; Max-Age=3600".to_owned()],
    ))
    .expect("capture response cookie");
    let current = block_on(selector.current_account(lease.account_id())).expect("current account");

    assert_eq!(outcome.credential_revision, Some(current.revision().get()));
    assert_ne!(current.revision(), lease.account().revision());
    block_on(selector.record_failure(&current, CodexAccountFailure::QuotaExhausted, None))
        .expect("record failure with current revision");
    assert_eq!(
        store
            .account("acct_primary")
            .expect("updated account")
            .quota()
            .access(),
        QuotaAccessState::Exhausted
    );
}
