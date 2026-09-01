use std::sync::Arc;

use gateway_core::account::ProviderAccountId;

use gateway_admin::{
    AdminServices,
    model::provider_credentials::{ImportCredentials, ProviderQuotaRequest},
    ports::provider::ProviderAdminErrorKind,
};

use super::accounts::{FakeAccountStore, FakeProviderAdmin, context, document, events, recorded};

#[tokio::test]
async fn xai_import_should_prepare_before_atomic_store_commit() {
    let events = events();
    let provider = FakeProviderAdmin::new("xai", events.clone());
    let store = FakeAccountStore::new("xai", events.clone());
    let services = service(provider.clone(), store.clone()).await;

    services
        .xai()
        .import_document(ImportCredentials {
            context: context("import-xai"),
            document: document(),
        })
        .await
        .expect("import xAI credential");

    assert_eq!(
        recorded(&events),
        [
            "provider.prepare_import",
            "store.commit_import",
            "provider.quota"
        ]
    );
    assert_eq!(
        provider.quota_requests(),
        [ProviderQuotaRequest {
            account_id: ProviderAccountId::new("acct_prepared").expect("account ID"),
            refresh: true,
            rolling_usage: None,
        }]
    );
    assert_eq!(store.audit_requests(), ["import-xai"]);
}

#[tokio::test]
async fn xai_import_should_refresh_quota_for_every_imported_account() {
    let events = events();
    let provider = FakeProviderAdmin::new("xai", events.clone());
    provider.set_import_account_ids(&["acct_first", "acct_second"]);
    let store = FakeAccountStore::new("xai", events);
    let services = service(provider.clone(), store).await;

    services
        .xai()
        .import_document(ImportCredentials {
            context: context("import-xai-batch"),
            document: document(),
        })
        .await
        .expect("import credentials");

    assert_eq!(
        provider.quota_requests(),
        [
            ProviderQuotaRequest {
                account_id: ProviderAccountId::new("acct_first").expect("first account ID"),
                refresh: true,
                rolling_usage: None,
            },
            ProviderQuotaRequest {
                account_id: ProviderAccountId::new("acct_second").expect("second account ID"),
                refresh: true,
                rolling_usage: None,
            },
        ]
    );
}

#[tokio::test]
async fn xai_import_should_remain_successful_when_quota_refresh_fails() {
    let events = events();
    let provider = FakeProviderAdmin::new("xai", events.clone());
    provider.fail_next_quota(ProviderAdminErrorKind::Unavailable);
    let store = FakeAccountStore::new("xai", events);
    let services = service(provider.clone(), store).await;

    let result = services
        .xai()
        .import_document(ImportCredentials {
            context: context("import-xai-quota-failure"),
            document: document(),
        })
        .await
        .expect("committed import remains successful");

    assert_eq!(result.credential_ids.len(), 1);
    assert_eq!(provider.quota_requests().len(), 1);
}

async fn service(provider: Arc<FakeProviderAdmin>, store: Arc<FakeAccountStore>) -> AdminServices {
    super::AdminHarness::new()
        .provider(provider)
        .accounts(store)
        .build()
        .await
}
