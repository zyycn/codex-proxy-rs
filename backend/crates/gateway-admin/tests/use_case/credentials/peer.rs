//! 验证凭据提交后的互联首查、原生额度分派与异步失败边界

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use gateway_admin::{
    model::provider_credentials::{ImportCredentials, ProviderQuota, ProviderQuotaReadRequest},
    ports::{
        peer::PeerUpstreamService,
        provider::{ProviderAdminError, ProviderAdminErrorKind},
    },
};
use gateway_core::{
    account::{ProviderAccount, ProviderAccountId, UpstreamInfoSource, UpstreamSchedulingSignal},
    error::StoreError,
    routing::ProviderKind,
};

use super::super::{
    AdminHarness,
    accounts::{FakeAccountStore, FakeProviderAdmin, account_record, context, document, events},
};

#[tokio::test]
async fn imported_peer_accounts_should_start_observation_without_waiting_for_network() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    provider.set_import_account_ids(&["acct_first", "acct_second"]);
    let mut first = account_record("openai");
    first.id = "acct_first".into();
    first.authentication_kind = "api_key".into();
    let mut second = first.clone();
    second.id = "acct_second".into();
    let store = FakeAccountStore::with_account(first.clone(), events);
    store.set_accounts(vec![first, second]);
    let peer = Arc::new(PendingPeer::default());
    let services = AdminHarness::new()
        .provider(provider.clone())
        .accounts(store)
        .upstream_info(peer.clone())
        .build()
        .await;

    let result = tokio::time::timeout(
        Duration::from_secs(1),
        services
            .credentials()
            .for_provider(&ProviderKind::new("openai").unwrap())
            .unwrap()
            .import_document(ImportCredentials {
                outbound_proxy_id: None,
                settings: None,
                context: context("import-peer"),
                document: document(),
            }),
    )
    .await
    .expect("import must not wait for peer response")
    .expect("import committed");
    peer.wait_for_requests(1).await;
    assert_eq!(peer.changed.lock().unwrap().len(), 2);
    assert_eq!(result.credential_ids.len(), 2);
    assert!(provider.quota_requests().is_empty());
    peer.gate.add_permits(2);
    peer.wait_for_requests(2).await;
    assert_eq!(
        *peer.requests.lock().unwrap(),
        ["acct_first", "acct_second"]
    );
    assert!(provider.quota_requests().is_empty());
}

#[tokio::test]
async fn native_import_should_use_provider_quota_when_peer_service_is_available() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    provider.set_import_account_ids(&["acct_test"]);
    let store = FakeAccountStore::new("openai", events);
    let peer = Arc::new(PendingPeer::default());
    let services = AdminHarness::new()
        .provider(provider.clone())
        .accounts(store)
        .upstream_info(peer.clone())
        .build()
        .await;

    services
        .credentials()
        .for_provider(&ProviderKind::new("openai").unwrap())
        .unwrap()
        .import_document(ImportCredentials {
            outbound_proxy_id: None,
            settings: None,
            context: context("import-native"),
            document: document(),
        })
        .await
        .expect("native import");
    provider.wait_for_quota_requests(1).await;
    assert!(peer.requests.lock().unwrap().is_empty());
    assert_eq!(
        provider.quota_requests()[0].account_id.as_str(),
        "acct_test"
    );
}

struct PendingPeer {
    changed: Mutex<Vec<String>>,
    requests: Mutex<Vec<String>>,
    started: tokio::sync::Notify,
    gate: tokio::sync::Semaphore,
}

impl Default for PendingPeer {
    fn default() -> Self {
        Self {
            changed: Mutex::default(),
            requests: Mutex::default(),
            started: tokio::sync::Notify::new(),
            gate: tokio::sync::Semaphore::new(0),
        }
    }
}

impl PendingPeer {
    async fn wait_for_requests(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let notified = self.started.notified();
                if self.requests.lock().unwrap().len() >= count {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("initial peer observation started");
    }
}

impl UpstreamInfoSource for PendingPeer {
    fn signal(
        &self,
        _: &ProviderAccount,
        _: Option<&str>,
        _: std::time::SystemTime,
    ) -> Option<UpstreamSchedulingSignal> {
        None
    }
}

#[async_trait]
impl PeerUpstreamService for PendingPeer {
    fn observed_concurrency_limit(
        &self,
        _: &gateway_core::account::QuotaObservationVersion,
        _: std::time::SystemTime,
    ) -> Option<gateway_core::account::AccountConcurrencyLimit> {
        None
    }

    async fn account_changed(&self, id: &ProviderAccountId) -> Result<(), StoreError> {
        self.changed.lock().unwrap().push(id.as_str().into());
        Ok(())
    }
    fn forget(&self, _: &ProviderAccountId) {}
    async fn refresh_quota(
        &self,
        id: &ProviderAccountId,
    ) -> Result<ProviderQuota, ProviderAdminError> {
        self.requests.lock().unwrap().push(id.as_str().into());
        self.started.notify_one();
        self.gate.acquire().await.unwrap().forget();
        Err(ProviderAdminError::new(ProviderAdminErrorKind::Unavailable))
    }
    async fn read_quota(&self, _: &ProviderAccountId) -> Result<Option<ProviderQuota>, StoreError> {
        panic!("unexpected cached read")
    }
    async fn read_summary(
        &self,
        _: &ProviderAccountId,
    ) -> Result<Option<ProviderQuota>, StoreError> {
        panic!("unexpected summary read")
    }
    async fn read_quotas(
        &self,
        _: &[ProviderQuotaReadRequest<'_>],
    ) -> Vec<Result<ProviderQuota, ProviderAdminError>> {
        panic!("unexpected batch read")
    }
}
