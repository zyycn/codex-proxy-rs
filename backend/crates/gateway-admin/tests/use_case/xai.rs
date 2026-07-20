use std::sync::Arc;

use gateway_core::{engine::credential::ProviderAccountId, routing::ProviderInstanceId};

use gateway_admin::{
    AdminServices,
    model::provider_credentials::{CredentialMutation, ImportCredentials},
};

use super::accounts::{
    FakeAccountStore, FakeProviderAdmin, context, document, events, recorded, revision,
};

#[tokio::test]
async fn xai_disable_should_commit_then_notify_the_stateless_provider() {
    let events = events();
    let provider = FakeProviderAdmin::new("xai", events.clone());
    let store = FakeAccountStore::new("xai", events.clone());
    let services = service(provider, store.clone()).await;

    services
        .xai()
        .disable(mutation("acct_test"))
        .await
        .expect("disable credential");

    assert_eq!(
        recorded(&events),
        [
            "store.credential_details",
            "store.set_enabled",
            "provider.account_unavailable",
        ]
    );
    assert_eq!(store.audit_requests(), ["request-xai"]);
}

#[tokio::test]
async fn xai_import_should_prepare_before_atomic_store_commit() {
    let events = events();
    let provider = FakeProviderAdmin::new("xai", events.clone());
    let store = FakeAccountStore::new("xai", events.clone());
    let services = service(provider, store.clone()).await;

    services
        .xai()
        .import_document(ImportCredentials {
            context: context("import-xai"),
            expected_config_revision: revision(3),
            provider_instance_id: ProviderInstanceId::new("inst_xai").expect("instance ID"),
            document: document(),
        })
        .await
        .expect("import xAI credential");

    assert_eq!(
        recorded(&events),
        ["provider.prepare_import", "store.commit_import"]
    );
    assert_eq!(store.audit_requests(), ["import-xai"]);
}

async fn service(provider: Arc<FakeProviderAdmin>, store: Arc<FakeAccountStore>) -> AdminServices {
    super::AdminHarness::new()
        .provider(provider)
        .accounts(store)
        .build()
        .await
}

fn mutation(account_id: &str) -> CredentialMutation {
    CredentialMutation {
        context: context("request-xai"),
        expected_config_revision: revision(1),
        account_id: ProviderAccountId::new(account_id).expect("account ID"),
    }
}
