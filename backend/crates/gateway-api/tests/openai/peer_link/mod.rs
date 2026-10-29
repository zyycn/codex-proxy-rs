//! 验证互联导出的授权范围、过期观测和独立于推理准入的只读合同

use crate::{
    admin::AdminTestFixture,
    support::{RAW_KEY, empty_request, key_fixture, response_json},
};
use axum::{
    Router,
    http::{Method, StatusCode, header},
    response::Response,
};
use chrono::{TimeDelta, Utc};
use gateway_admin::model::{
    Revision,
    account_groups::{AccountGroupColor, AccountGroupRef},
    accounts::{AccountCapacity, AccountPageItem, AccountRecord},
    provider_credentials::{
        ProviderQuota, ProviderQuotaWindow, ProviderQuotaWindowRole, QuotaLocalUsageAttribution,
    },
};
use gateway_core::{
    account::{
        AccountStatusFacts, CredentialState, ProviderAccountId, QuotaEvidence, QuotaState,
        resolve_account_status,
    },
    engine::{execution::DefaultExecutionService, provider::ProviderRegistry},
    policy::{ClientApiKeyId, ClientPeerSharing, ClientPolicy, PlaintextClientApiKey, RateLimits},
    routing::{
        AccountGroupId, ClientRoutingScope, ConfigRevision, FrozenAccountScope, ModelCapabilities,
        ProviderKind, ProviderModel, RoutingGroupSnapshot, RuntimeAccount, RuntimeAccountDirectory,
        RuntimeSnapshot, UpstreamModelId,
    },
    runtime::RuntimeSnapshotHandle,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tower::ServiceExt as _;

const PATH: &str = "/v1/cpr/link";

fn sharing() -> ClientPeerSharing {
    ClientPeerSharing {
        enabled: true,
        share_pool: true,
        share_quota: true,
        share_plans: false,
    }
}

async fn fixture(sharing: ClientPeerSharing, plaintext: &str) -> (AdminTestFixture, Router) {
    let fixture = key_fixture().await;
    let now = Utc::now();
    let group = AccountGroupRef {
        id: AccountGroupId::new(format!("grp_{}", "1".repeat(32))).unwrap(),
        name: "private-group-sentinel".into(),
        color: AccountGroupColor::parse("#2563EBFF").unwrap(),
        enabled: true,
    };
    let id = ClientApiKeyId::new("key-42").unwrap();
    let mut key = fixture
        .services
        .client_keys()
        .reveal(&id)
        .await
        .unwrap()
        .record;
    key.peer_sharing = sharing;
    key.groups = vec![group.clone()];
    key.name = "private-key-sentinel".into();
    key.budget.limits.daily_usd = "1".parse().unwrap();
    key.budget.daily_used_usd = "2".parse().unwrap();
    *fixture.client_key.lock().unwrap() = Some(key);
    let provider = ProviderKind::new("openai").unwrap();
    let facts = AccountStatusFacts {
        enabled: true,
        credential_state: CredentialState::Ready,
        access_token_expires_at: None,
        quota: QuotaState::allowed(now.into()),
        cooldown: None,
        last_error_reason: None,
        last_error_message: None,
    };
    *fixture.account.lock().unwrap() = Some(AccountPageItem {
        protection_rules: Vec::new(),
        capacity: AccountCapacity {
            used_slots: None,
            total_slots: None,
        },
        projection: resolve_account_status(&facts, now.into()),
        account: AccountRecord {
            id: "acct_private_sentinel".into(),
            provider_kind: provider,
            groups: vec![group.clone()],
            name: "private-account-sentinel".into(),
            notes: None,
            email: Some("private-email-sentinel@example.invalid".into()),
            upstream_user_id: None,
            upstream_account_id: None,
            plan_type: Some("pro".into()),
            authentication_kind: "oauth".into(),
            credential_revision: Revision::new(1).unwrap(),
            has_refresh_token: true,
            access_token_expires_at: None,
            next_refresh_at: None,
            enabled: true,
            concurrency_limit: None,
            weight: Default::default(),
            model_access: Default::default(),
            outbound_proxy_id: None,
            outbound_proxy: None,
            outbound_proxy_enabled: true,
            credential_state: facts.credential_state,
            credential_observed_at: now,
            quota: facts.quota,
            last_error_reason: None,
            last_error_message: None,
            created_at: now,
            updated_at: now,
        },
    });
    *fixture.provider_quota.lock().unwrap() = Some(ProviderQuota {
        upstream: None,
        plan_type: Some("pro".into()),
        observed_at: Some(now),
        refresh_token_expires_at: None,
        windows: vec![ProviderQuotaWindow {
            upstream: None,
            key: "weekly".into(),
            group: "shortTerm".into(),
            label: "周额度".into(),
            limit_id: None,
            limit_name: None,
            role: Some(ProviderQuotaWindowRole::Secondary),
            local_usage_attribution: QuotaLocalUsageAttribution::AccountWide,
            window_seconds: Some(604800),
            used_percent: Some(47.0),
            reset_at: Some(now + TimeDelta::days(2)),
            limit_reached: false,
            local_usage: None,
            provider_data: None,
        }],
        credits: None,
        limit_reached: false,
        provider_data: None,
    });
    let app = router_with_sharing(&fixture, sharing, plaintext);
    (fixture, app)
}

fn router_with_sharing(
    fixture: &AdminTestFixture,
    sharing: ClientPeerSharing,
    plaintext: &str,
) -> Router {
    let group = fixture.client_key.lock().unwrap().as_ref().unwrap().groups[0].clone();
    let provider = ProviderKind::new("openai").unwrap();
    let id = ClientApiKeyId::new("key-42").unwrap();
    let directory = Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
        ProviderAccountId::new("acct_private_sentinel").unwrap(),
        RuntimeAccount::new(provider.clone(), BTreeSet::from([group.id.clone()]))
            .with_direct_authentication_kind(Some("oauth".into())),
    )])));
    let scope = FrozenAccountScope::new(
        directory.clone(),
        ClientRoutingScope::restricted(
            vec![RoutingGroupSnapshot::new(
                group.id.clone(),
                "test".to_owned(),
            )],
            BTreeSet::from([group.id]),
            BTreeSet::from([provider.clone()]),
        )
        .unwrap(),
    );
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        gateway_core::settings::SettingsValues::new(2, "smart", Default::default(), None, None),
        vec![provider.clone()],
        vec![ProviderModel::new(
            provider,
            UpstreamModelId::new("model-a").unwrap(),
            ModelCapabilities::new(
                BTreeSet::from([gateway_core::operation::OperationKind::Generate]),
                Some(16000),
            ),
        )],
        vec![
            ClientPolicy::new(
                id,
                PlaintextClientApiKey::new(plaintext).unwrap(),
                Arc::new(scope),
                true,
                RateLimits::unlimited(),
            )
            .with_peer_sharing(sharing),
        ],
    )
    .unwrap()
    .with_account_directory(directory);
    let execution = Arc::new(DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(snapshot),
        Arc::new(super::UnusedExecutionStore),
        ProviderRegistry::default(),
        Arc::new(super::UnusedAdmissions),
        Arc::new(super::UnusedContinuation),
        Arc::new(super::IgnoredClientApiKeyUsage),
        Arc::new(crate::support::RecordingDiagnostics::default()),
    ));
    super::api_router_with_admin_and_execution(fixture.services.clone(), execution)
}

async fn query(app: &Router, path: &str, plaintext: Option<&str>) -> Response {
    let mut request = empty_request(Method::GET, path);
    if let Some(key) = plaintext {
        request.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {key}").parse().unwrap(),
        );
    }
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    response
}

#[tokio::test]
async fn exports_scoped_observations_even_when_the_key_budget_is_exhausted_without_writing_usage() {
    let (fixture, app) = fixture(sharing(), RAW_KEY).await;
    let before = fixture.client_key.lock().unwrap().clone();
    let response = query(&app, PATH, Some(RAW_KEY)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["object"], "cpr.peer");
    assert_eq!(body["usage"]["data"]["daily"]["remaining"], "0");
    assert_eq!(body["pool"]["account_count"], 1);
    assert!(body["pool"].get("plans").is_none());
    assert_eq!(
        body["quota"]["accounts"][0]["windows"][0]["used_percent"],
        47.0
    );
    assert_eq!(body["models"][0]["state"], "available");
    assert_eq!(body["models"][0]["coverage"], "complete");
    assert!(!body.to_string().contains("sentinel"));
    assert!(!body.to_string().contains(RAW_KEY));
    assert_eq!(*fixture.client_key.lock().unwrap(), before);
    assert!(fixture.observations.lock().unwrap().summaries.is_empty());
}

#[tokio::test]
async fn requires_authentication_explicit_sharing_and_bound_groups_and_rejects_query_overrides() {
    let (fixture, app) = fixture(sharing(), RAW_KEY).await;
    assert_eq!(
        query(&app, PATH, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        query(&app, &format!("{PATH}?account=private"), Some(RAW_KEY))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    fixture
        .client_key
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .peer_sharing
        .enabled = false;
    assert_eq!(
        query(&app, PATH, Some(RAW_KEY)).await.status(),
        StatusCode::FORBIDDEN
    );
    {
        let mut key = fixture.client_key.lock().unwrap();
        let key = key.as_mut().unwrap();
        key.peer_sharing.enabled = true;
        key.groups.clear();
    }
    assert_eq!(
        query(&app, PATH, Some(RAW_KEY)).await.status(),
        StatusCode::FORBIDDEN
    );
    fixture.client_key.lock().unwrap().as_mut().unwrap().enabled = false;
    assert_eq!(
        query(&app, PATH, Some(RAW_KEY)).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn disabled_or_changed_group_bindings_remove_accounts_from_the_frozen_scope() {
    let (fixture, app) = fixture(sharing(), RAW_KEY).await;
    let original_group = fixture.client_key.lock().unwrap().as_ref().unwrap().groups[0].clone();
    fixture.client_key.lock().unwrap().as_mut().unwrap().groups[0].enabled = false;
    let response = query(&app, PATH, Some(RAW_KEY)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["pool"]["account_count"], 0);

    {
        let mut key = fixture.client_key.lock().unwrap();
        let group = &mut key.as_mut().unwrap().groups[0];
        *group = original_group.clone();
        group.id = AccountGroupId::new(format!("grp_{}", "2".repeat(32))).unwrap();
    }
    let body = response_json(query(&app, PATH, Some(RAW_KEY)).await).await;
    assert_eq!(body["pool"]["account_count"], 0);
    fixture.client_key.lock().unwrap().as_mut().unwrap().groups[0] = original_group;
    let body = response_json(query(&app, PATH, Some(RAW_KEY)).await).await;
    assert_eq!(body["pool"]["account_count"], 1);
}

#[tokio::test]
async fn current_field_permissions_and_direct_account_facts_narrow_the_frozen_scope() {
    let (fixture, app) = fixture(sharing(), RAW_KEY).await;
    fixture
        .client_key
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .peer_sharing
        .share_quota = false;
    let body = response_json(query(&app, PATH, Some(RAW_KEY)).await).await;
    assert_eq!(body["quota"], json!({ "disclosure": "none" }));
    assert!(body.get("models").is_none());
    fixture
        .account
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .account
        .authentication_kind = "api_key".into();
    let body = response_json(query(&app, PATH, Some(RAW_KEY)).await).await;
    assert_eq!(body["pool"]["account_count"], 0);
}

#[tokio::test]
async fn stale_observations_remain_visible_but_cannot_claim_exhausted_model_coverage() {
    let (fixture, app) = fixture(sharing(), RAW_KEY).await;
    let old = Utc::now() - TimeDelta::hours(1);
    fixture
        .provider_quota
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .observed_at = Some(old);
    fixture
        .account
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .account
        .quota = QuotaState::exhausted(QuotaEvidence::ProviderDenied, old.into(), None);
    let body = response_json(query(&app, PATH, Some(RAW_KEY)).await).await;
    assert_eq!(
        body["quota"]["accounts"][0]["observed_at"],
        old.to_rfc3339()
    );
    assert_eq!(body["models"][0]["state"], "unknown");
    assert_eq!(body["models"][0]["coverage"], "partial");
    fixture
        .account
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .account
        .quota = QuotaState::exhausted(QuotaEvidence::ProviderDenied, Utc::now().into(), None);
    let body = response_json(query(&app, PATH, Some(RAW_KEY)).await).await;
    assert_eq!(body["models"][0]["state"], "exhausted");
    assert_eq!(body["models"][0]["coverage"], "complete");
}

#[tokio::test]
async fn account_identifiers_are_isolated_per_key_and_peer_queries_have_separate_rate_limits() {
    let (_, app) = fixture(sharing(), RAW_KEY).await;
    let body = response_json(query(&app, PATH, Some(RAW_KEY)).await).await;
    let (_, other) = fixture(sharing(), "different-peer-key").await;
    let other_body = response_json(query(&other, PATH, Some("different-peer-key")).await).await;
    assert_ne!(
        body["quota"]["accounts"][0]["id"],
        other_body["quota"]["accounts"][0]["id"]
    );
    for _ in 1..30 {
        assert_eq!(
            query(&app, PATH, Some(RAW_KEY)).await.status(),
            StatusCode::OK
        );
    }
    let response = query(&app, PATH, Some(RAW_KEY)).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(response.headers().contains_key(header::RETRY_AFTER));
    assert_eq!(
        query(&app, "/v1/usage", Some(RAW_KEY)).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn oversized_snapshots_are_rejected_without_returning_a_truncated_pool() {
    let (fixture, app) = fixture(sharing(), RAW_KEY).await;
    let window = fixture
        .provider_quota
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .windows[0]
        .clone();
    fixture
        .provider_quota
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .windows = vec![window.clone(); gateway_protocol::peer::MAX_QUOTA_WINDOWS + 1];
    let response = query(&app, PATH, Some(RAW_KEY)).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response_json(response).await.get("pool").is_none());
    let mut oversized = window;
    oversized.label = "x".repeat(gateway_protocol::peer::MAX_RESPONSE_BYTES);
    fixture
        .provider_quota
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .windows = vec![oversized];
    assert_eq!(
        query(&app, PATH, Some(RAW_KEY)).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn heavy_snapshots_reject_excess_concurrency_and_release_slots_on_completion_or_cancellation()
{
    let (fixture, app) = fixture(sharing(), RAW_KEY).await;
    let gate = Arc::new(crate::admin::ProviderQuotaGate {
        entered: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
    });
    *fixture.provider_quota_gate.lock().unwrap() = Some(gate.clone());
    let mut requests = Vec::new();
    for _ in 0..8 {
        let app = app.clone();
        requests.push(tokio::spawn(async move {
            query(&app, PATH, Some(RAW_KEY)).await
        }));
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        gate.entered.acquire_many(8),
    )
    .await
    .expect("eight snapshots entered quota aggregation")
    .unwrap()
    .forget();

    let current_key = fixture.client_key.lock().unwrap().take();
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        query(&app, PATH, Some(RAW_KEY)),
    )
    .await
    .expect("excess snapshot must not wait for a slot");
    *fixture.client_key.lock().unwrap() = current_key;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()[header::RETRY_AFTER], "1");
    assert_eq!(
        response_json(response).await["error"]["code"],
        "peer_query_rate_limited"
    );

    assert_eq!(
        query(&app, "/v1/usage", Some(RAW_KEY)).await.status(),
        StatusCode::OK
    );
    let budget_app = router_with_sharing(
        &fixture,
        ClientPeerSharing {
            enabled: true,
            ..Default::default()
        },
        RAW_KEY,
    );
    let budget = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        query(&budget_app, PATH, Some(RAW_KEY)),
    )
    .await
    .expect("budget-only queries do not consume heavy-query slots");
    assert_eq!(budget.status(), StatusCode::OK);
    assert_eq!(response_json(budget).await["pool"]["disclosure"], "none");
    let cancelled = requests.pop().unwrap();
    cancelled.abort();
    assert!(cancelled.await.unwrap_err().is_cancelled());
    let app_for_replacement = app.clone();
    requests.push(tokio::spawn(async move {
        query(&app_for_replacement, PATH, Some(RAW_KEY)).await
    }));
    tokio::time::timeout(std::time::Duration::from_secs(2), gate.entered.acquire())
        .await
        .expect("cancelled snapshot released its slot")
        .unwrap()
        .forget();

    gate.release.add_permits(8);
    for request in requests {
        assert_eq!(request.await.unwrap().status(), StatusCode::OK);
    }
    gate.release.add_permits(1);
    assert_eq!(
        query(&app, PATH, Some(RAW_KEY)).await.status(),
        StatusCode::OK
    );
}
