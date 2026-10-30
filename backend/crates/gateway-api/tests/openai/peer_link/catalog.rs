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

struct CatalogFixture(SnapshotFacts);

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
            Ok(vec![
                ProviderModelCapabilities::new(
                    UpstreamModelId::new("model-a").unwrap(),
                    ModelCapabilities::new([OperationKind::Generate].into(), None),
                )
                .with_catalog_accounts(
                    [ProviderAccountId::new("acct_catalog_source").unwrap()].into(),
                ),
            ])
        })
    }
}

#[tokio::test]
async fn native_quota_coverage_uses_routing_eligibility_and_current_model_permissions() {
    let (fixture, _) = fixture(sharing(), RAW_KEY).await;
    let group = fixture.client_key.lock().unwrap().as_ref().unwrap().groups[0].clone();
    let account_ids = ["acct_catalog_source", "acct_private_sentinel"]
        .map(|id| ProviderAccountId::new(id).unwrap());
    let facts = Arc::new(CatalogFixture(SnapshotFacts::new(
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
    )));
    let snapshot = RuntimeSnapshotCompiler::new(facts.clone(), facts)
        .compile()
        .await
        .unwrap();
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
