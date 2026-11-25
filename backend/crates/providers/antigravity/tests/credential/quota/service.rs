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
