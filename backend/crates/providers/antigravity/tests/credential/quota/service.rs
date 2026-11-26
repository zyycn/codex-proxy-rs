//! 真实分组额度投影、模型独立调度与迟到观测隔离

use crate::support::Fixture;
use gateway_core::{
    account::{CredentialRevision, CredentialState, ProviderAccount, QuotaState},
    lifecycle::CancellationToken,
};
use serde_json::json;
use std::time::{Duration, SystemTime};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

async fn upstream(server: &MockServer, remaining: serde_json::Value, delay: Duration) {
    Mock::given(path("/v1internal:loadCodeAssist")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"cloudaicompanionProject":"p","currentTier":{"id":"free-tier","name":"Antigravity"},"paidTier":{"id":"g1-pro-tier","name":"Google AI Pro"}}))).mount(server).await;
    Mock::given(path("/v1internal:fetchAvailableModels")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"models":{"fixture-model":{"displayName":"Gemini","quotaInfo":{"remainingFraction":remaining,"resetTime":"2099-01-01T00:00:00Z"}},"other-model":{"displayName":"Claude","quotaInfo":{"remainingFraction":0.8,"resetTime":"2099-01-01T00:00:00Z"}}}}))).mount(server).await;
    Mock::given(path("/v1internal:retrieveUserQuotaSummary")).respond_with(ResponseTemplate::new(200).set_delay(delay).set_body_json(json!({"groups":[{"displayName":"Gemini Models","buckets":[{"bucketId":"gemini-weekly","displayName":"Weekly","window":"weekly","remainingFraction":0.9,"resetTime":"2099-01-01T00:00:00Z"},{"bucketId":"gemini-5h","displayName":"Five hour","window":"5h","remainingFraction":remaining,"resetTime":"2099-01-01T00:00:00Z"}]},{"displayName":"Claude and GPT models","buckets":[{"bucketId":"3p-weekly","displayName":"Weekly","window":"weekly","remainingFraction":0.8,"resetTime":"2099-01-01T00:00:00Z"}]}]}))).mount(server).await;
}

#[tokio::test]
async fn window_usage_requires_every_named_member_to_resolve_in_the_observed_catalog() {
    use gateway_admin::model::provider_credentials::QuotaLocalUsageAttribution::{
        Models, Unavailable,
    };
    let server = MockServer::start().await;
    Mock::given(path("/v1internal:loadCodeAssist"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"cloudaicompanionProject":"p"})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/v1internal:fetchAvailableModels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models":{
            "gemini-4-neon-high":{"displayName":"Gemini 4 Neon (High)"},
            "gemini-4-neon-low":{"displayName":"Gemini 4 Neon (Low)"},
            "orchestrator-alias":{"displayName":"Orchestrator (Neon)"},
            "gemini-lite":{"displayName":"Gemini Lite"},
            "gpt-oss-medium":{"displayName":"GPT-OSS (Medium)"}
        }})))
        .mount(&server)
        .await;
    let group = |description: Option<&str>, name: &str| {
        json!({
            "displayName":name, "description":description,
            "buckets":(["5h", "weekly"].map(|window| json!({
                "bucketId":format!("{name}-{window}"), "displayName":window, "window":window
            })))
        })
    };
    Mock::given(path("/v1internal:retrieveUserQuotaSummary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"groups":[
            group(Some("Models within this group: Gemini 4 Neon, Orchestrator (Neon)"), "gemini"),
            group(Some("Models within this group: GPT-OSS"), "3p"),
            group(Some("Models within this group: Gemini Lite, Missing Model"), "partial"),
            group(None, "unknown")
        ]})))
        .mount(&server)
        .await;
    let f = Fixture::new(&server.uri());
    let account = f.store.primary().loaded.lock().unwrap().account.clone();
    f.quota.refresh(account.id()).await.unwrap();
    let view = f.quota.read(account.id()).await.unwrap();
    for (group, attribution) in [
        (
            "gemini",
            Models(
                [
                    "gemini-4-neon-high",
                    "gemini-4-neon-low",
                    "orchestrator-alias",
                ]
                .map(str::to_owned)
                .into(),
            ),
        ),
        ("3p", Models(["gpt-oss-medium".to_owned()].into())),
        ("partial", Unavailable),
        ("unknown", Unavailable),
    ] {
        for window in ["5h", "weekly"] {
            let key = format!("{group}-{window}");
            let observed = view.windows.iter().find(|w| w.key == key).unwrap();
            assert_eq!(observed.local_usage_attribution, attribution);
        }
    }
}

#[tokio::test]
async fn background_refresh_requires_new_generation_and_an_aged_snapshot() {
    let server = MockServer::start().await;
    upstream(&server, json!(0.5), Duration::ZERO).await;
    let f = Fixture::new(&server.uri());
    let account = f.store.primary();
    let cancel = CancellationToken::new();
    f.quota.reconcile(&cancel).await.unwrap();
    let initial = account.quota.lock().unwrap().clone().unwrap();
    let queries = || async {
        server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path() == "/v1internal:retrieveUserQuotaSummary")
            .count()
    };
    assert_eq!(queries().await, 1);

    *account.last_generation.lock().unwrap() = Some(SystemTime::now());
    f.quota.reconcile(&cancel).await.unwrap();
    assert_eq!(queries().await, 1);

    let old = initial.observed_at - Duration::from_secs(900);
    account.quota.lock().unwrap().as_mut().unwrap().observed_at = old;
    *account.last_generation.lock().unwrap() = Some(old - Duration::from_secs(1));
    f.quota.read(&initial.account_id).await.unwrap();
    f.quota.reconcile(&cancel).await.unwrap();
    assert_eq!(queries().await, 1);

    *account.last_generation.lock().unwrap() = Some(SystemTime::now());
    f.quota.reconcile(&cancel).await.unwrap();
    f.quota.reconcile(&cancel).await.unwrap();
    assert_eq!(queries().await, 2);
    f.quota.refresh(&initial.account_id).await.unwrap();
    assert_eq!(queries().await, 3);
}

#[tokio::test]
async fn exhausted_quota_is_rechecked_when_due_even_without_generation() {
    use gateway_core::account::QuotaEvidence;
    let server = MockServer::start().await;
    upstream(&server, json!(0.5), Duration::ZERO).await;
    let f = Fixture::new(&server.uri());
    let account = f.store.primary();
    let cancel = CancellationToken::new();
    f.quota.reconcile(&cancel).await.unwrap();
    let now = SystemTime::now();
    let old = now - Duration::from_secs(600);
    for (observed, reset, due) in [
        (old, Some(now + Duration::from_secs(300)), false),
        (old, Some(now), true),
        (now, None, false),
        (old, None, true),
    ] {
        {
            let mut loaded = account.loaded.lock().unwrap();
            loaded.account = loaded.account.clone().with_account_facts(
                true,
                CredentialState::Ready,
                QuotaState::exhausted(QuotaEvidence::UsageLimitReached, observed, reset),
                None,
                None,
            );
        }
        f.quota.reconcile(&cancel).await.unwrap();
        assert_eq!(
            account
                .loaded
                .lock()
                .unwrap()
                .account
                .quota()
                .is_exhausted(),
            !due
        );
    }
}

#[tokio::test]
async fn quota_and_catalog_refresh_use_current_global_identity() {
    let server = MockServer::start().await;
    upstream(&server, json!(0.5), Duration::ZERO).await;
    let f = Fixture::new(&server.uri());
    let account = f.store.primary().loaded.lock().unwrap().account.clone();
    for (version, changelist) in [("1.5.14", ""), ("1.5.15", "; cl=1027820220")] {
        *f.store.request_profile.lock().unwrap() = Some(crate::profile::object(
            json!({"client":"cli","platform":"linux","versionMode":"fixed","clientVersion":version,"arch":null}),
        ));
        let before = server.received_requests().await.unwrap().len();
        f.quota.refresh(account.id()).await.unwrap();
        f.catalog.account_models(account.id(), true).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len() - before, 5);
        for request in &requests[before..] {
            assert_eq!(
                request.headers["user-agent"],
                format!(
                    "antigravity/cli/{version} (aidev_client; os_type=linux; arch=amd64{changelist}; auth_method=consumer)"
                )
            );
        }
    }
    *f.store.request_profile.lock().unwrap() =
        Some(crate::profile::object(json!({"client":"unsupported"})));
    assert!(f.quota.refresh(account.id()).await.is_err());
    assert!(f.catalog.account_models(account.id(), true).await.is_err());
    assert_eq!(server.received_requests().await.unwrap().len(), 10);
}

#[tokio::test]
async fn quota_refresh_projects_paid_plan_without_exhausting_other_models() {
    let server = MockServer::start().await;
    upstream(&server, json!(0.0), Duration::ZERO).await;
    let f = Fixture::new(&server.uri());
    let account = f.store.primary().loaded.lock().unwrap().account.clone();
    f.quota.refresh(account.id()).await.unwrap();
    let view = f.quota.read(account.id()).await.unwrap();
    assert_eq!(view.plan_type.as_deref(), Some("g1-pro-tier"));
    assert_eq!(view.windows.len(), 3);
    assert!(
        view.windows
            .iter()
            .all(|window| window.local_usage.is_none())
    );
    assert_eq!(view.windows[1].used_percent, Some(100.0));
    assert!(!view.limit_reached);
    assert_eq!(
        f.store
            .primary()
            .quota
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .state
            .access(),
        gateway_core::account::QuotaAccessState::Allowed
    );
    let gemini = f
        .quota
        .scheduling(std::slice::from_ref(&account), "fixture-model")
        .await
        .unwrap();
    let claude = f
        .quota
        .scheduling(std::slice::from_ref(&account), "other-model")
        .await
        .unwrap();
    assert!(gemini[account.id()].0);
    assert!(!claude[account.id()].0);
    assert_eq!(
        claude[account.id()].1.as_ref().unwrap().remaining_rank(),
        Some(8000)
    );
    {
        let mut old = f.store.primary().quota.lock().unwrap();
        old.as_mut().unwrap().observed_at = SystemTime::now() - Duration::from_secs(601);
    }
    let expired = f
        .quota
        .scheduling(std::slice::from_ref(&account), "fixture-model")
        .await
        .unwrap();
    assert!(!expired[account.id()].0);
    assert!(expired[account.id()].1.is_none());
}

#[tokio::test]
async fn unknown_quota_preserves_only_observed_windows_without_inventing_percentages() {
    let server = MockServer::start().await;
    upstream(&server, json!(null), Duration::ZERO).await;
    let f = Fixture::new(&server.uri());
    let account = f.store.primary().loaded.lock().unwrap().account.clone();
    let empty = f.quota.read(account.id()).await.unwrap();
    assert!(empty.windows.is_empty());
    f.quota.refresh(account.id()).await.unwrap();
    let view = f.quota.read(account.id()).await.unwrap();
    assert_eq!(view.windows[1].used_percent, None);
    assert!(
        !f.quota
            .scheduling(std::slice::from_ref(&account), "fixture-model")
            .await
            .unwrap()[account.id()]
        .0
    );
}

#[tokio::test]
async fn failed_or_stale_refresh_preserves_persisted_quota() {
    let server = MockServer::start().await;
    upstream(&server, json!(0.5), Duration::ZERO).await;
    let f = Fixture::new(&server.uri());
    let account = f.store.primary().loaded.lock().unwrap().account.clone();
    f.quota.refresh(account.id()).await.unwrap();
    let first = f.store.primary().quota.lock().unwrap().clone().unwrap();
    server.reset().await;
    Mock::given(path("/v1internal:loadCodeAssist"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    assert!(f.quota.refresh(account.id()).await.is_err());
    assert_eq!(
        f.store.primary().quota.lock().unwrap().clone().unwrap(),
        first
    );
    server.reset().await;
    upstream(&server, json!(0.0), Duration::from_millis(150)).await;
    let service = f.quota.clone();
    let id = account.id().clone();
    let pending = tokio::spawn(async move { service.refresh(&id).await });
    for _ in 0..100 {
        if server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.url.path() == "/v1internal:retrieveUserQuotaSummary")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    f.store.primary().loaded.lock().unwrap().account = ProviderAccount::new(
        account.id().clone(),
        account.provider().clone(),
        "Fixture".into(),
        account.upstream_user_id().map(str::to_owned),
        "oauth".into(),
        CredentialRevision::new(2).unwrap(),
        account.access_token_expires_at(),
    )
    .with_account_facts(
        true,
        CredentialState::Ready,
        QuotaState::unknown(),
        None,
        None,
    );
    assert!(pending.await.unwrap().is_err());
    assert_eq!(
        f.store.primary().quota.lock().unwrap().clone().unwrap(),
        first
    );
    assert!(
        f.quota
            .read(account.id())
            .await
            .unwrap()
            .plan_type
            .is_none()
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    f.quota.reconcile(&cancelled).await.unwrap();
    f.quota.reconcile(&CancellationToken::new()).await.unwrap();
    assert_eq!(
        f.quota
            .read(account.id())
            .await
            .unwrap()
            .plan_type
            .as_deref(),
        Some("g1-pro-tier")
    );
}

#[tokio::test]
async fn peer_model_facts_keep_membership_quota_and_revision_separate() {
    use gateway_core::{
        account::QuotaAccessState, engine::provider::Provider, error::ProviderErrorKind,
    };
    let server = MockServer::start().await;
    upstream(&server, json!(0.0), Duration::ZERO).await;
    let f = Fixture::new(&server.uri());
    let account = crate::support::account_record();
    let id = f
        .store
        .primary()
        .loaded
        .lock()
        .unwrap()
        .account
        .id()
        .clone();
    f.quota.refresh(&id).await.unwrap();
    let facts = f
        .quota
        .peer_model_facts(std::slice::from_ref(&account))
        .await
        .unwrap();
    let models = &facts[&account.id].models;
    assert_eq!(
        models["fixture-model"].unwrap().state.access(),
        QuotaAccessState::Exhausted
    );
    assert_eq!(
        models["other-model"].unwrap().state.access(),
        QuotaAccessState::Allowed
    );
    assert!(!models.contains_key("unavailable-model"));
    let result = f
        .provider
        .clone()
        .execute(
            crate::support::request(false),
            crate::support::context(
                crate::profile::Selection::default()
                    .resolve("2.34.0")
                    .frozen(),
                CancellationToken::new(),
            ),
        )
        .await;
    assert!(matches!(result, Err(e) if e.kind() == ProviderErrorKind::QuotaExhausted));
    f.store
        .primary()
        .quota
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .observed_at = SystemTime::now() - Duration::from_secs(601);
    let stale = f
        .quota
        .peer_model_facts(std::slice::from_ref(&account))
        .await
        .unwrap();
    assert!(stale[&account.id].models["fixture-model"].is_none());
    let mut changed = account;
    changed.credential_revision = gateway_admin::model::Revision::new(2).unwrap();
    assert!(
        f.quota
            .peer_model_facts(&[changed])
            .await
            .unwrap()
            .is_empty()
    );
}
