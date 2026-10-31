//! 账号出站代理管理、占用释放与探测结果的用例测试

use async_trait::async_trait;
use gateway_admin::{
    model::{MutationContext, Revision, proxies::*},
    ports::{
        proxy::{ProxyProbe, ProxyStore},
        store::AdminStoreResult,
    },
};
use gateway_core::account::{OutboundProxy, ProviderAccountId};

#[derive(Default)]
pub(super) struct TestProxies {
    pub events: Option<super::accounts::EventLog>,
    pub accounts: Option<Vec<ProxyAccountRef>>,
    pub record: Option<ProxyRecord>,
    pub records: Vec<ProxyRecord>,
}

struct ImportGuard(super::accounts::EventLog);

impl gateway_admin::ports::proxy::ProxyImportGuard for ImportGuard {}

impl Drop for ImportGuard {
    fn drop(&mut self) {
        self.0.lock().unwrap().push("proxy.release");
    }
}

#[async_trait]
impl ProxyStore for TestProxies {
    async fn remove_account(
        &self,
        _: &str,
        _: &ProviderAccountId,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(super::unavailable("proxy"))
    }

    async fn reserve_import(
        &self,
        id: &str,
    ) -> AdminStoreResult<gateway_admin::ports::proxy::ProxyImportReservation> {
        let events = self
            .events
            .as_ref()
            .ok_or_else(|| super::unavailable("proxy"))?;
        events.lock().unwrap().push("proxy.reserve");
        Ok(gateway_admin::ports::proxy::ProxyImportReservation {
            binding: ImportProxyBinding {
                id: id.to_owned(),
                proxy: OutboundProxy::parse("http://127.0.0.1:8080").unwrap(),
            },
            guard: Box::new(ImportGuard(events.clone())),
        })
    }
    async fn list(&self, _: ProxyListQuery) -> AdminStoreResult<ProxyPage> {
        Err(super::unavailable("proxy"))
    }
    async fn list_accounts(
        &self,
        query: ProxyAccountListQuery,
    ) -> AdminStoreResult<ProxyAccountPage> {
        let items = self
            .accounts
            .clone()
            .ok_or_else(|| super::unavailable("proxy"))?;
        Ok(ProxyAccountPage {
            total: items.len() as u64,
            items,
            page: query.page,
            page_size: query.page_size.get(),
        })
    }
    async fn get(&self, id: &str) -> AdminStoreResult<ProxyRecord> {
        self.record
            .clone()
            .filter(|record| record.id == id)
            .or_else(|| self.records.iter().find(|record| record.id == id).cloned())
            .ok_or_else(|| super::unavailable("proxy"))
    }
    async fn create(&self, _: NewProxy, _: &MutationContext) -> AdminStoreResult<ProxyMutation> {
        if let Some(events) = &self.events {
            events.lock().unwrap().push("proxy.create");
        }
        Err(super::unavailable("proxy"))
    }
    async fn update(&self, _: UpdateProxy, _: &MutationContext) -> AdminStoreResult<ProxyMutation> {
        Err(super::unavailable("proxy"))
    }
    async fn delete(
        &self,
        _: &str,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(super::unavailable("proxy"))
    }
    async fn record_test(
        &self,
        id: &str,
        _: Revision,
        result: ProxyTestResult,
        _: &MutationContext,
    ) -> AdminStoreResult<ProxyMutation> {
        let mut record = self.get(id).await?;
        record.last_test = Some(result);
        Ok(ProxyMutation {
            config_revision: record.revision,
            record,
        })
    }
}

#[async_trait]
impl ProxyProbe for TestProxies {
    async fn test(&self, _: Option<&OutboundProxy>, _: bool) -> ProxyTestResult {
        panic!("unexpected proxy probe")
    }
}

#[tokio::test]
async fn authorization_uses_selected_proxy_regardless_of_probe_status() {
    use super::accounts::{FakeAccountStore, FakeProviderAdmin, context, events, recorded};
    use gateway_admin::model::provider_credentials::StartAuthorization;
    use std::sync::Arc;

    for kind in ["openai", "xai"] {
        for probe_success in [None, Some(false), Some(true)] {
            let events = events();
            let now = chrono::Utc::now();
            let services = super::AdminHarness::new()
                .provider(FakeProviderAdmin::new(kind, events.clone()))
                .accounts(FakeAccountStore::new(kind, events.clone()))
                .proxies(Arc::new(TestProxies {
                    record: Some(ProxyRecord {
                        enabled: true,
                        auto_location: false,
                        detected_location: None,
                        location: None,
                        id: "proxy_oauth".to_owned(),
                        name: "授权出口".to_owned(),
                        proxy: OutboundProxy::parse("http://127.0.0.1:8080").unwrap(),
                        revision: Revision::new(1).unwrap(),
                        account_count: 0,
                        last_test_at: probe_success.map(|_| now),
                        last_test: probe_success.map(|success| ProxyTestResult {
                            location: Default::default(),
                            success,
                            latency_ms: 10,
                            exit_ip: None,
                            exit_ipv4: None,
                            exit_ipv6: None,
                            message: "出口探测结果".to_owned(),
                        }),
                        created_at: now,
                        updated_at: now,
                    }),
                    ..Default::default()
                }))
                .build()
                .await;
            let command = StartAuthorization {
                outbound_proxy: Some(AccountProxySelection::Saved("proxy_oauth".to_owned())),
                context: context("oauth-proxy-status"),
                name: "授权账号".to_owned(),
                reauthorization: None,
            };
            let result = if kind == "openai" {
                services
                    .credentials()
                    .for_provider(&gateway_core::routing::ProviderKind::new("openai").unwrap())
                    .unwrap()
                    .start_authorization(command)
                    .await
            } else {
                services
                    .credentials()
                    .for_provider(&gateway_core::routing::ProviderKind::new("xai").unwrap())
                    .unwrap()
                    .start_authorization(command)
                    .await
            };
            assert!(result.is_ok(), "{kind}, {probe_success:?}: {result:?}");
            assert_eq!(recorded(&events), ["provider.start_authorization"]);
        }
    }
}

#[tokio::test]
async fn linked_accounts_share_plan_resolution_and_only_read_cached_quota() {
    use super::accounts::{FakeProviderAdmin, events};
    use gateway_admin::model::{PageSize, provider_credentials::ProviderQuota};
    use std::sync::Arc;

    for (stored, cached, expected) in [
        (None, Some("free"), Some("free")),
        (Some("unknown"), Some("free"), Some("free")),
        (Some("plus"), Some("free"), Some("plus")),
        (None, None, None),
    ] {
        let provider = FakeProviderAdmin::new("openai", events());
        provider.set_quota(ProviderQuota {
            upstream: None,
            credits: None,
            plan_type: cached.map(str::to_owned),
            observed_at: None,
            refresh_token_expires_at: None,
            windows: vec![],
            limit_reached: false,
            provider_data: None,
        });
        let services = super::AdminHarness::new()
            .provider(provider.clone())
            .proxies(Arc::new(TestProxies {
                accounts: Some(vec![ProxyAccountRef {
                    upstream: None,
                    id: "acct_plan".to_owned(),
                    name: "套餐测试".to_owned(),
                    email: None,
                    provider_kind: "openai".to_owned(),
                    authentication_kind: "oauth".to_owned(),
                    plan_type: stored.map(str::to_owned),
                    plan_type_display: None,
                    groups: vec![],
                    enabled: true,
                }]),
                ..Default::default()
            }))
            .build()
            .await;
        let result = services
            .proxies()
            .list_accounts(ProxyAccountListQuery {
                proxy_id: "proxy_plan".to_owned(),
                page: 1,
                page_size: PageSize::new(20).unwrap(),
                search: String::new(),
            })
            .await
            .unwrap();
        assert_eq!(result.items[0].plan_type.as_deref(), expected);
        assert_eq!(
            result.items[0].plan_type_display.as_deref(),
            match expected {
                Some("free") => Some("openai display: free"),
                Some("plus") => Some("openai display: plus"),
                _ => None,
            }
        );
        let requests = provider.quota_requests();
        assert_eq!(requests.len(), usize::from(stored != Some("plus")));
        assert!(requests.iter().all(|request| !request.refresh));
    }
}

#[tokio::test]
async fn credential_import_keeps_proxy_reserved_until_commit_and_releases_on_errors() {
    use super::accounts::{
        FakeAccountStore, FakeProviderAdmin, context, document, events, recorded,
    };
    use gateway_admin::model::provider_credentials::ImportCredentials;
    use gateway_admin::ports::provider::ProviderAdminErrorKind;
    use std::sync::Arc;

    for kind in ["openai", "xai"] {
        for failure in [None, Some("prepare"), Some("commit")] {
            let events = events();
            let provider = FakeProviderAdmin::new(kind, events.clone());
            let store = FakeAccountStore::new(kind, events.clone());
            if failure == Some("prepare") {
                provider.fail_next(ProviderAdminErrorKind::Unavailable);
            }
            if failure == Some("commit") {
                store.fail_next_commit();
            }
            let services = super::AdminHarness::new()
                .provider(provider)
                .accounts(store)
                .proxies(Arc::new(TestProxies {
                    events: Some(events.clone()),
                    ..Default::default()
                }))
                .build()
                .await;
            let command = ImportCredentials {
                outbound_proxy_id: Some("proxy_import".to_owned()),
                settings: None,
                context: context("reserved-import"),
                document: document(),
            };
            let result = if kind == "openai" {
                services
                    .credentials()
                    .for_provider(&gateway_core::routing::ProviderKind::new("openai").unwrap())
                    .unwrap()
                    .import_document(command)
                    .await
            } else {
                services
                    .credentials()
                    .for_provider(&gateway_core::routing::ProviderKind::new("xai").unwrap())
                    .unwrap()
                    .import_document(command)
                    .await
            };
            assert_eq!(result.is_err(), failure.is_some());
            let events = recorded(&events);
            let expected = if failure == Some("prepare") {
                vec!["proxy.reserve", "provider.prepare_import", "proxy.release"]
            } else {
                vec![
                    "proxy.reserve",
                    "provider.prepare_import",
                    "store.commit_import",
                    "proxy.release",
                ]
            };
            assert_eq!(&events[..expected.len()], expected);
        }
    }
}

#[tokio::test]
async fn batch_create_stops_after_uncertain_store_failure_without_replaying() {
    use super::accounts::{context, events, recorded};
    use gateway_admin::model::AdminErrorKind;
    let events = events();
    let services = super::AdminHarness::new()
        .proxies(std::sync::Arc::new(TestProxies {
            events: Some(events.clone()),
            ..Default::default()
        }))
        .build()
        .await;
    let commands = [" ", "First", "Skipped"]
        .into_iter()
        .map(|name| {
            Ok(NewProxy {
                name: name.to_owned(),
                proxy: OutboundProxy::parse("http://proxy.example:8080").unwrap(),
                auto_location: false,
                location: None,
                test: None,
            })
        })
        .collect();
    let results = services
        .proxies()
        .create_batch(commands, &context("batch-create"))
        .await
        .unwrap();
    assert!(
        matches!(&results[0], ProxyBatchResult::Completed(Err(error)) if error.kind() == AdminErrorKind::Invalid)
    );
    assert!(
        matches!(&results[1], ProxyBatchResult::Completed(Err(error)) if error.kind() == AdminErrorKind::Unavailable)
    );
    assert!(matches!(&results[2], ProxyBatchResult::Skipped));
    assert_eq!(recorded(&events), vec!["proxy.create"]);
}

struct GatedProxyProbe {
    started: tokio::sync::mpsc::UnboundedSender<()>,
    release: tokio::sync::Semaphore,
    active: std::sync::atomic::AtomicUsize,
}

struct ActiveProbe<'a>(&'a std::sync::atomic::AtomicUsize);
impl Drop for ActiveProbe<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[async_trait]
impl ProxyProbe for GatedProxyProbe {
    async fn test(&self, _: Option<&OutboundProxy>, _: bool) -> ProxyTestResult {
        self.active
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _active = ActiveProbe(&self.active);
        self.started.send(()).unwrap();
        self.release.acquire().await.unwrap().forget();
        ProxyTestResult {
            location: Default::default(),
            success: true,
            latency_ms: 1,
            exit_ip: None,
            exit_ipv4: None,
            exit_ipv6: None,
            message: "ok".to_owned(),
        }
    }
}

#[tokio::test]
async fn batch_tests_share_capacity_preserve_order_and_release_slots_on_cancellation() {
    use super::accounts::context;
    use std::sync::{Arc, atomic::Ordering};
    let (started, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let probe = Arc::new(GatedProxyProbe {
        started,
        release: tokio::sync::Semaphore::new(0),
        active: Default::default(),
    });
    let now = chrono::Utc::now();
    let records = (0..4)
        .map(|i| ProxyRecord {
            id: format!("proxy_{i}"),
            name: format!("Proxy {i}"),
            proxy: OutboundProxy::parse("http://proxy.example:8080").unwrap(),
            revision: Revision::new(1).unwrap(),
            enabled: true,
            auto_location: false,
            location: None,
            detected_location: None,
            account_count: 0,
            last_test_at: None,
            last_test: None,
            created_at: now,
            updated_at: now,
        })
        .collect();
    let mut harness = super::AdminHarness::new().proxies(Arc::new(TestProxies {
        records,
        ..Default::default()
    }));
    harness.proxy_probe = probe.clone();
    let services = harness.build().await;
    let commands = || {
        (0..4)
            .map(|i| {
                Ok(TestProxy {
                    id: format!("proxy_{i}"),
                    revision: Revision::new(1).unwrap(),
                    detect_location: true,
                })
            })
            .collect()
    };
    let context = context("batch-test");
    let mut batch = Box::pin(services.proxies().test_batch(commands(), &context));
    tokio::select! {
        _ = &mut batch => panic!("batch finished before probes were released"),
        _ = async { receiver.recv().await.unwrap(); receiver.recv().await.unwrap(); } => {}
    }
    assert_eq!(probe.active.load(Ordering::Relaxed), 2);
    let mut single_a = Box::pin(services.proxies().probe(None, false));
    let mut single_b = Box::pin(services.proxies().probe(None, false));
    tokio::select! {
        _ = &mut batch => panic!("batch unexpectedly finished"),
        _ = &mut single_a => panic!("single probe unexpectedly finished"),
        _ = &mut single_b => panic!("single probe unexpectedly finished"),
        _ = async { receiver.recv().await.unwrap(); receiver.recv().await.unwrap(); } => {}
    }
    assert_eq!(probe.active.load(Ordering::Relaxed), 4);
    assert_eq!(
        services
            .proxies()
            .probe(None, false)
            .await
            .unwrap_err()
            .kind(),
        gateway_admin::model::AdminErrorKind::RateLimited
    );
    drop((batch, single_a, single_b));
    assert_eq!(probe.active.load(Ordering::Relaxed), 0);
    probe.release.add_permits(4);
    let results = services
        .proxies()
        .test_batch(commands(), &context)
        .await
        .unwrap();
    for (index, result) in results.into_iter().enumerate() {
        let ProxyBatchResult::Completed(Ok(record)) = result else {
            panic!("probe failed after cancellation")
        };
        assert_eq!(record.id, format!("proxy_{index}"));
        assert!(record.last_test.unwrap().success);
    }
    assert_eq!(probe.active.load(Ordering::Relaxed), 0);
}
