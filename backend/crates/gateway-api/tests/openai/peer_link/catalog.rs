//! 原生目录来源仅用于展示，互联额度覆盖仍按账号的模型候选资格计算

use super::*;
use futures::future::BoxFuture;
use gateway_core::{
    account::{AccountModelAccess, AccountModelAccessMode},
    operation::OperationKind,
    routing::{
        ProviderCatalogGeneration, ProviderCatalogPort, ProviderCatalogUnavailable,
        ProviderModelCapabilities,
        snapshot::{
            RuntimeSnapshotCompiler, SnapshotAccountGroupFacts, SnapshotAccountGroupMemberFacts,
            SnapshotClientPolicyFacts, SnapshotFacts, SnapshotProviderAccountFacts,
            SnapshotStoreError, SnapshotStorePort,
        },
    },
};

struct CatalogFixture(SnapshotFacts, usize);

impl SnapshotStorePort for CatalogFixture {
    fn load_snapshot_facts(&self) -> BoxFuture<'_, Result<SnapshotFacts, SnapshotStoreError>> {
        Box::pin(async { Ok(self.0.clone()) })
    }

    fn current_config_revision(&self) -> BoxFuture<'_, Result<ConfigRevision, SnapshotStoreError>> {
        Box::pin(async { Ok(ConfigRevision::new(1).unwrap()) })
    }
}

impl ProviderCatalogPort for CatalogFixture {
    fn is_direct_account(&self, _: &ProviderKind, authentication_kind: &str) -> bool {
        authentication_kind == "oauth"
    }

    fn catalog_generations(&self) -> BTreeMap<ProviderKind, ProviderCatalogGeneration> {
        [(
            ProviderKind::new("openai").unwrap(),
            ProviderCatalogGeneration::new(1),
        )]
        .into()
    }

    fn query_model_capabilities(
        &self,
        _: &ProviderKind,
    ) -> BoxFuture<'_, Result<Vec<ProviderModelCapabilities>, ProviderCatalogUnavailable>> {
        Box::pin(async {
            Ok((0..self.1)
                .map(|index| {
                    ProviderModelCapabilities::new(
                        UpstreamModelId::new(if index == 0 {
                            "model-a".to_owned()
                        } else {
                            format!("model-{index}")
                        })
                        .unwrap(),
                        ModelCapabilities::new([OperationKind::Generate].into(), None),
                    )
                    .with_catalog_accounts(
                        [ProviderAccountId::new("acct_catalog_source").unwrap()].into(),
                    )
                })
                .collect())
        })
    }
}

#[tokio::test]
async fn native_quota_coverage_uses_routing_eligibility_and_current_model_permissions() {
    let (fixture, _) = fixture(sharing(), RAW_KEY).await;
    let snapshot = catalog_snapshot(&fixture, 1).await;
    let app = router_with_snapshot(&fixture, snapshot);

    // 目录来源账号当前不可读，另一个未贡献目录的账号仍有可用观测，不能被漏掉
    let body = response_json(query(&app, PATH, Some(RAW_KEY)).await).await;
    assert_eq!(
        body["quota"]["accounts"][0]["models"],
        json!(["alias", "model-a"])
    );
    for model in body["models"].as_array().unwrap() {
        assert_eq!(model["state"], "available");
        assert_eq!(model["coverage"], "partial");
    }

    fixture
        .account
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .account
        .model_access =
        AccountModelAccess::new(AccountModelAccessMode::Denylist, vec!["model-a".into()]).unwrap();
    let body = response_json(query(&app, PATH, Some(RAW_KEY)).await).await;
    assert_eq!(body["quota"]["accounts"][0]["models"], json!([]));
    for model in body["models"].as_array().unwrap() {
        assert_eq!(model["state"], "unknown");
        assert_eq!(model["coverage"], "partial");
    }
}

async fn catalog_snapshot(fixture: &AdminTestFixture, models: usize) -> RuntimeSnapshot {
    let group = fixture.client_key.lock().unwrap().as_ref().unwrap().groups[0].clone();
    let account_ids = ["acct_catalog_source", "acct_private_sentinel"]
        .map(|id| ProviderAccountId::new(id).unwrap());
    let facts = Arc::new(CatalogFixture(
        SnapshotFacts::new(
            ConfigRevision::new(1).unwrap(),
            ConfigRevision::new(1).unwrap(),
            gateway_core::settings::SettingsValues::new(
                2,
                "smart",
                [("alias".into(), "model-a".into())].into(),
                None,
                None,
            ),
            vec![
                SnapshotClientPolicyFacts::new(
                    ClientApiKeyId::new("key-42").unwrap(),
                    PlaintextClientApiKey::new(RAW_KEY).unwrap(),
                    vec![group.id.clone()],
                    RateLimits::unlimited(),
                )
                .with_peer_sharing(sharing()),
            ],
            vec![SnapshotAccountGroupFacts::new(
                group.id.clone(),
                group.name,
                true,
            )],
            account_ids
                .iter()
                .map(|id| {
                    SnapshotProviderAccountFacts::new(id.clone(), "openai")
                        .with_authentication_kind("oauth".into())
                })
                .collect(),
            account_ids
                .iter()
                .map(|id| SnapshotAccountGroupMemberFacts::new(group.id.clone(), id.clone()))
                .collect(),
        ),
        models,
    ));
    RuntimeSnapshotCompiler::new(facts.clone(), facts)
        .compile()
        .await
        .unwrap()
}

#[tokio::test]
async fn native_catalog_accepts_the_model_limit_and_rejects_overflow() {
    let (fixture, _) = fixture(sharing(), RAW_KEY).await;
    // 目录还公开一个 alias，边界按公开名称总数计算
    for (count, expected) in [
        (gateway_protocol::peer::MAX_MODELS - 1, StatusCode::OK),
        (
            gateway_protocol::peer::MAX_MODELS,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    ] {
        let app = router_with_snapshot(&fixture, catalog_snapshot(&fixture, count).await);
        let response = query(&app, PATH, Some(RAW_KEY)).await;
        assert_eq!(response.status(), expected);
        if expected == StatusCode::OK {
            let bytes = axum::body::to_bytes(
                response.into_body(),
                gateway_protocol::peer::MAX_RESPONSE_BYTES,
            )
            .await
            .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                body["models"].as_array().unwrap().len(),
                gateway_protocol::peer::MAX_MODELS
            );
        } else {
            assert!(response_json(response).await.get("pool").is_none());
        }
    }
}

#[tokio::test]
async fn model_expansion_honors_the_byte_budget_and_current_permissions() {
    use gateway_admin::{PeerLinkError, model::peer_link::PeerLinkLimits};
    use gateway_core::engine::execution::ExecutionService as _;
    let (fixture, _) = fixture(sharing(), RAW_KEY).await;
    let execution = execution_with_snapshot(catalog_snapshot(&fixture, 1).await);
    let client = execution.authenticate(RAW_KEY).unwrap();
    let limits = PeerLinkLimits {
        max_accounts: 3000,
        max_models: 2000,
        max_quota_windows: 6000,
        max_response_bytes: 12,
    };
    let snapshot = fixture
        .services
        .peer_link()
        .snapshot(&client, limits)
        .await
        .unwrap();
    assert_eq!(snapshot.quota.unwrap()[0].models, ["alias", "model-a"]);
    let limits = PeerLinkLimits {
        max_response_bytes: 11,
        ..limits
    };
    assert!(matches!(
        fixture.services.peer_link().snapshot(&client, limits).await,
        Err(PeerLinkError::SnapshotTooLarge)
    ));
    fixture
        .account
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .account
        .model_access =
        AccountModelAccess::new(AccountModelAccessMode::Denylist, vec!["model-a".into()]).unwrap();
    let snapshot = fixture
        .services
        .peer_link()
        .snapshot(&client, limits)
        .await
        .unwrap();
    assert!(snapshot.quota.unwrap()[0].models.is_empty());
    assert!(
        snapshot
            .models
            .iter()
            .all(|model| model.state == gateway_admin::model::peer_link::PeerModelState::Unknown)
    );
}
