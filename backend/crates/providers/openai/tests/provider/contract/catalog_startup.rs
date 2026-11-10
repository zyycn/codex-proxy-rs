//! 验证大账号池与慢目录上游不会阻塞启动时的能力读取

use super::*;
use gateway_core::engine::provider::{Provider, ProviderRegistry};
use gateway_core::routing::snapshot::{
    RuntimeSnapshotCompiler, SnapshotFacts, SnapshotProviderAccountFacts, SnapshotStoreError,
    SnapshotStorePort,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;
use wiremock::{Request, Respond};

struct StartupStore(SnapshotFacts);

impl SnapshotStorePort for StartupStore {
    fn load_snapshot_facts(&self) -> BoxFuture<'_, Result<SnapshotFacts, SnapshotStoreError>> {
        Box::pin(async { Ok(self.0.clone()) })
    }

    fn current_config_revision(&self) -> BoxFuture<'_, Result<ConfigRevision, SnapshotStoreError>> {
        Box::pin(async { Ok(self.0.config_revision()) })
    }
}

#[tokio::test]
async fn cold_catalog_with_ten_thousand_accounts_never_fetches_during_capability_reads() {
    let store = Arc::new(MemoryAccountStore::default());
    let mut accounts = Vec::new();
    for index in 0..10_000 {
        let id = format!("acct_startup_{index:05}");
        create_account(&store, &id).await;
        accounts.push(SnapshotProviderAccountFacts::new(
            ProviderAccountId::new(id).unwrap(),
            "openai",
        ));
    }
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_delay(Duration::from_secs(30)))
        .expect(0)
        .mount(&server)
        .await;
    let revision = ConfigRevision::new(1).unwrap();
    let facts = Arc::new(StartupStore(SnapshotFacts::new(
        revision,
        revision,
        SettingsValues::new(3, "smart", BTreeMap::new(), None, None),
        vec![],
        vec![],
        accounts,
        vec![],
    )));

    // 重建 Provider 模拟重启；Core 的读取只报告目录未知，不触发全量账号扫描
    for _ in 0..3 {
        let provider = provider_with_base_url(&store, server.uri());
        let error =
            tokio::time::timeout(Duration::from_secs(1), provider.query_model_capabilities())
                .await
                .expect("startup capability read must not wait for upstream")
                .expect_err("a cold catalog is unknown");
        assert_eq!(error.kind(), ProviderErrorKind::Unavailable);
        let registry = Arc::new(ProviderRegistry::new([provider as Arc<dyn Provider>]).unwrap());
        let compiler = RuntimeSnapshotCompiler::new(facts.clone(), registry);
        let snapshot = tokio::time::timeout(Duration::from_secs(5), compiler.compile())
            .await
            .expect("startup snapshot must not wait for the account scan")
            .expect("cold catalog must not prevent snapshot publication");
        assert!(snapshot.public_models().is_empty());
    }
    server.verify().await;
}

struct SlowCatalog {
    calls: Arc<AtomicUsize>,
    first_batch: Arc<Notify>,
}

impl Respond for SlowCatalog {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == 32 {
            self.first_batch.notify_one();
        }
        ResponseTemplate::new(200)
            .set_body_raw(OFFICIAL_FIXTURE.to_vec(), "application/json")
            .set_delay(Duration::from_secs(30))
    }
}

#[tokio::test]
async fn slow_background_catalog_is_bounded_and_does_not_block_capability_reads() {
    let store = Arc::new(MemoryAccountStore::default());
    for index in 0..64 {
        create_account(&store, &format!("acct_background_{index:02}")).await;
    }
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let first_batch = Arc::new(Notify::new());
    Mock::given(method("GET"))
        .respond_with(SlowCatalog {
            calls: Arc::clone(&calls),
            first_batch: Arc::clone(&first_batch),
        })
        .mount(&server)
        .await;
    let (provider, _, _, catalog) = provider_and_quota_with_runtime_ports(
        &store,
        Arc::new(MemorySessionAffinity::default()),
        server.uri(),
        Arc::new(TestLeaseCoordinator::default()),
        u32::try_from(DEFAULT_STREAM_MAX_RETRIES).unwrap(),
        Arc::new(MemoryCooldownPort::new()),
        crate::support::runtime_policy(),
    );
    let refresh = {
        let catalog = Arc::clone(&catalog);
        tokio::spawn(async move { catalog.refresh_catalogs().await })
    };
    tokio::time::timeout(Duration::from_secs(5), first_batch.notified())
        .await
        .expect("background scan must make progress beyond four accounts");
    assert_eq!(calls.load(Ordering::SeqCst), 32);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), provider.query_model_capabilities(),)
            .await
            .expect("capability read must not join a slow refresh")
            .is_err()
    );

    // 取消整轮扫描后刷新锁必须释放，下一轮后台任务仍能完成
    refresh.abort();
    assert!(refresh.await.unwrap_err().is_cancelled());
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(OFFICIAL_FIXTURE.to_vec(), "application/json"),
        )
        .expect(64)
        .mount(&server)
        .await;
    tokio::time::timeout(Duration::from_secs(5), catalog.refresh_catalogs())
        .await
        .expect("refresh cancellation must release the lock")
        .expect("catalog refresh");
    let generation = provider.catalog_generation();
    for _ in 0..3 {
        assert!(provider.query_model_capabilities().await.is_ok());
    }
    assert_eq!(provider.catalog_generation(), generation);
    server.verify().await;
}

#[tokio::test]
async fn issue_666_same_plan_special_model_remains_visible_and_routes_only_to_its_source_after_restart()
 {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    let source = "acct_provider_contract";
    let other = "acct_scope_new";
    for (id, models) in [
        (source, vec!["common", "special-666"]),
        (other, vec!["common"]),
    ] {
        create_account(&store, id).await;
        Mock::given(method("GET")).and(path("/codex/models")).and(header("chatgpt-account-id",format!("chatgpt-{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models":models.iter().map(|id|json!({"slug":id,"display_name":id})).collect::<Vec<_>>()})))
            .expect(1).mount(&server).await;
    }
    // 调度本来优先选 B，只有 A 自己的目录能授权特殊模型
    store.set_scheduling(
        other,
        None,
        gateway_core::account::AccountWeight::new(100).unwrap(),
    );
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("chatgpt-account-id", format!("chatgpt-{source}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream"),
        )
        .expect(2)
        .mount(&server)
        .await;
    for restarted in [false, true] {
        let (provider, _, _, catalog) = provider_and_quota_with_runtime_ports(
            &store,
            Arc::new(MemorySessionAffinity::default()),
            server.uri(),
            Arc::new(TestLeaseCoordinator::default()),
            u32::try_from(DEFAULT_STREAM_MAX_RETRIES).unwrap(),
            Arc::new(MemoryCooldownPort::new()),
            crate::support::runtime_policy(),
        );
        catalog.restore().await.unwrap();
        if !restarted {
            assert!(provider.query_model_capabilities().await.is_err());
        }
        catalog.refresh_catalogs().await.unwrap();
        assert!(
            catalog
                .client_model_catalog(&contract_account_scope())
                .await
                .unwrap()
                .iter()
                .any(|m| m.model.as_str() == "special-666")
        );
        let sources = catalog.cached().unwrap().unwrap();
        assert_eq!(
            sources.model_catalog_accounts()["special-666"],
            std::collections::BTreeSet::from([gateway_core::account::ProviderAccountId::new(
                source
            )
            .unwrap()])
        );
        let op = Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(
                "openai",
                json!({"model":"special-666","input":"hello"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap()
            .with_context(Map::from_iter([("use_websocket".into(), json!(false))])),
        ));
        let mut stream = provider
            .execute(
                planned_request_for_model("openai", op, "special-666"),
                context("req_issue_666", CancellationToken::new()),
            )
            .await
            .unwrap();
        assert_eq!(stream.metadata().provider_account_id().as_str(), source);
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }
    server.verify().await;
}
