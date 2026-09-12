use std::sync::Arc;

use gateway_core::account::ProviderAccountId;

use gateway_admin::{
    AdminServices,
    model::provider_credentials::{
        CompleteAuthorization, ImportCredentials, ProviderQuotaRequest, StartAuthorization,
    },
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
            outbound_proxy_id: None,
            settings: Some(super::accounts::import_settings()),
            context: context("import-xai"),
            document: document(),
        })
        .await
        .expect("import xAI credential");

    provider.wait_for_quota_requests(1).await;
    assert_eq!(
        recorded(&events),
        [
            "provider.prepare_import",
            "store.commit_import",
            "provider.account_facts_changed",
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
    assert_eq!(
        store.import_settings(),
        [Some(super::accounts::import_settings())]
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
            outbound_proxy_id: None,
            settings: None,
            context: context("import-xai-batch"),
            document: document(),
        })
        .await
        .expect("import credentials");

    provider.wait_for_quota_requests(2).await;
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
            outbound_proxy_id: None,
            settings: None,
            context: context("import-xai-quota-failure"),
            document: document(),
        })
        .await
        .expect("committed import remains successful");

    provider.wait_for_quota_requests(1).await;
    assert_eq!(result.credential_ids.len(), 1);
    assert_eq!(provider.quota_requests().len(), 1);
}

#[tokio::test]
async fn xai_authorization_should_observe_quota_without_affecting_commit() {
    for quota_failure in [None, Some(ProviderAdminErrorKind::Unavailable)] {
        let events = events();
        let provider = FakeProviderAdmin::new("xai", events.clone());
        if let Some(kind) = quota_failure {
            provider.fail_next_quota(kind);
        }
        let store = FakeAccountStore::new("xai", events.clone());
        let services = service(provider.clone(), store.clone()).await;
        services
            .xai()
            .start_authorization(StartAuthorization {
                outbound_proxy: None,
                context: context("oauth-xai-create"),
                name: "new xAI credential".to_owned(),
                reauthorization: None,
            })
            .await
            .expect("start authorization");
        let result = services
            .xai()
            .complete_authorization(CompleteAuthorization {
                settings: None,
                context: context("oauth-xai-create"),
                flow_id: "flow-test".to_owned(),
                callback_url: "http://localhost/callback?code=test&state=test".to_owned(),
            })
            .await
            .expect("quota observation must not fail authorization");

        provider.wait_for_quota_requests(1).await;
        assert_eq!(
            recorded(&events),
            [
                "provider.start_authorization",
                "provider.complete_authorization",
                "store.commit_authorization",
                "provider.account_facts_changed",
                "provider.quota",
            ]
        );
        assert_eq!(
            provider.quota_requests(),
            [ProviderQuotaRequest {
                account_id: result.account_id,
                refresh: true,
                rolling_usage: None,
            }]
        );
        assert_eq!(store.audit_requests(), ["oauth-xai-create"]);
    }
}

#[tokio::test]
async fn credential_import_should_return_while_initial_quota_is_pending() {
    for kind in ["openai", "xai"] {
        let events = events();
        let provider = FakeProviderAdmin::new(kind, events.clone());
        let quota_release = provider.pause_next_quota();
        let store = FakeAccountStore::new(kind, events);
        let services = service(provider.clone(), store).await;
        let command = ImportCredentials {
            outbound_proxy_id: None,
            settings: None,
            context: context("import-slow-quota"),
            document: document(),
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            match kind {
                "openai" => services.openai().import_document(command).await,
                _ => services.xai().import_document(command).await,
            }
        })
        .await
        .expect("committed import must not wait for quota")
        .expect("import credentials");
        provider.wait_for_quota_requests(1).await;
        assert_eq!(result.credential_ids.len(), 1);
        quota_release.send(()).expect("finish quota observation");
    }
}

#[tokio::test]
async fn xai_import_store_failure_should_not_observe_quota_or_invalidate_provider_facts() {
    let events = events();
    let provider = FakeProviderAdmin::new("xai", events.clone());
    let store = FakeAccountStore::new("xai", events.clone());
    store.fail_next_commit();
    let services = service(provider.clone(), store).await;

    services
        .xai()
        .import_document(ImportCredentials {
            outbound_proxy_id: None,
            settings: None,
            context: context("import-xai-store-failure"),
            document: document(),
        })
        .await
        .expect_err("store commit fails");

    assert_eq!(
        recorded(&events),
        ["provider.prepare_import", "store.commit_import"]
    );
    assert!(provider.quota_requests().is_empty());
}

async fn service(provider: Arc<FakeProviderAdmin>, store: Arc<FakeAccountStore>) -> AdminServices {
    super::AdminHarness::new()
        .provider(provider)
        .accounts(store)
        .build()
        .await
}
