use std::sync::Arc;

use gateway_core::engine::credential::ProviderAccountId;

use gateway_admin::{
    AdminServices,
    model::provider_credentials::{
        AuthorizationMutationTarget, CompleteAuthorization, CredentialDeletion, CredentialMutation,
        ImportCredentials, ProviderQuotaRequest, StartAuthorization,
    },
    ports::provider::ProviderAdminErrorKind,
};

use super::accounts::{
    FakeAccountStore, FakeProviderAdmin, context, document, events, recorded, revision,
};

#[tokio::test]
async fn openai_enable_should_use_scoped_store_without_provider_mutation() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    let store = FakeAccountStore::new("openai", events.clone());
    let services = service(provider, store.clone()).await;

    services
        .openai()
        .enable(mutation("acct_test"))
        .await
        .expect("enable credential");

    assert_eq!(
        recorded(&events),
        ["store.credential_details", "store.set_enabled"]
    );
    assert_eq!(store.audit_requests(), ["request-openai"]);
}

#[tokio::test]
async fn openai_disable_should_commit_then_release_provider_resources() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    let store = FakeAccountStore::new("openai", events.clone());
    let services = service(provider, store.clone()).await;

    services
        .openai()
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
    assert_eq!(store.audit_requests(), ["request-openai"]);
}

#[tokio::test]
async fn openai_delete_should_commit_then_release_provider_resources() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    let store = FakeAccountStore::new("openai", events.clone());
    let services = service(provider, store.clone()).await;

    services
        .openai()
        .delete(deletion("acct_test"))
        .await
        .expect("delete credential");

    assert_eq!(
        recorded(&events),
        [
            "store.credential_details",
            "store.delete",
            "provider.account_unavailable",
        ]
    );
    assert_eq!(store.audit_requests(), ["request-openai"]);
}

#[tokio::test]
async fn openai_disable_store_failure_should_not_release_provider_resources() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    let store = FakeAccountStore::new("openai", events.clone());
    store.fail_next_commit();
    let services = service(provider, store).await;

    services
        .openai()
        .disable(mutation("acct_test"))
        .await
        .expect_err("failed Store transaction");

    assert_eq!(
        recorded(&events),
        ["store.credential_details", "store.set_enabled"]
    );
}

#[tokio::test]
async fn openai_import_should_prepare_before_atomic_store_commit() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    let store = FakeAccountStore::new("openai", events.clone());
    let services = service(provider.clone(), store.clone()).await;

    services
        .openai()
        .import_document(ImportCredentials {
            context: context("import-openai"),
            document: document(),
        })
        .await
        .expect("import credential");

    assert_eq!(
        recorded(&events),
        [
            "provider.prepare_import",
            "store.commit_import",
            "provider.quota",
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
    assert_eq!(store.audit_requests(), ["import-openai"]);
}

#[tokio::test]
async fn openai_import_should_refresh_quota_for_every_imported_account() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    provider.set_import_account_ids(&["acct_first", "acct_second"]);
    let store = FakeAccountStore::new("openai", events);
    let services = service(provider.clone(), store).await;

    let result = services
        .openai()
        .import_document(ImportCredentials {
            context: context("import-openai-batch"),
            document: document(),
        })
        .await
        .expect("import credentials");

    assert_eq!(
        result.credential_ids,
        [
            ProviderAccountId::new("acct_first").expect("first account ID"),
            ProviderAccountId::new("acct_second").expect("second account ID"),
        ]
    );
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
async fn openai_import_should_remain_successful_when_quota_refresh_fails() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    provider.fail_next_quota(ProviderAdminErrorKind::Unavailable);
    let store = FakeAccountStore::new("openai", events);
    let services = service(provider.clone(), store).await;

    let result = services
        .openai()
        .import_document(ImportCredentials {
            context: context("import-openai-quota-failure"),
            document: document(),
        })
        .await
        .expect("committed import remains successful");

    assert_eq!(
        result.credential_ids,
        [ProviderAccountId::new("acct_prepared").expect("account ID")]
    );
    assert_eq!(provider.quota_requests().len(), 1);
}

#[tokio::test]
async fn openai_authorization_create_should_observe_initial_quota() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    let store = FakeAccountStore::new("openai", events.clone());
    let services = service(provider.clone(), store.clone()).await;

    services
        .openai()
        .start_authorization(StartAuthorization {
            context: context("oauth-start-openai-create"),
            name: "new OpenAI credential".to_owned(),
            reauthorization: None,
        })
        .await
        .expect("start authorization");
    services
        .openai()
        .complete_authorization(CompleteAuthorization {
            context: context("oauth-start-openai-create"),
            flow_id: "flow-test".to_owned(),
            callback_url: "http://localhost/callback?code=test&state=test".to_owned(),
        })
        .await
        .expect("complete authorization");

    assert_eq!(
        recorded(&events),
        [
            "provider.start_authorization",
            "provider.complete_authorization",
            "store.commit_authorization",
            "provider.quota",
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
    assert_eq!(store.audit_requests(), ["oauth-start-openai-create"]);
}

#[tokio::test]
async fn openai_authorization_create_should_remain_successful_when_initial_quota_fails() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    provider.fail_next_quota(ProviderAdminErrorKind::Unavailable);
    let store = FakeAccountStore::new("openai", events);
    let services = service(provider.clone(), store).await;

    services
        .openai()
        .start_authorization(StartAuthorization {
            context: context("oauth-start-openai-quota-failure"),
            name: "new OpenAI credential".to_owned(),
            reauthorization: None,
        })
        .await
        .expect("start authorization");
    let result = services
        .openai()
        .complete_authorization(CompleteAuthorization {
            context: context("oauth-start-openai-quota-failure"),
            flow_id: "flow-test".to_owned(),
            callback_url: "http://localhost/callback?code=test&state=test".to_owned(),
        })
        .await
        .expect("quota observation must not fail authorization");

    assert_eq!(result.account_id.as_str(), "acct_prepared");
    assert_eq!(provider.quota_requests().len(), 1);
}

#[tokio::test]
async fn openai_authorization_store_failure_should_release_claim_for_retry() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    provider.retry_authorization_after_abort();
    let store = FakeAccountStore::new("openai", events.clone());
    store.fail_next_commit();
    let services = service(provider.clone(), store).await;
    let command = || CompleteAuthorization {
        context: context("oauth-store-retry"),
        flow_id: "flow-test".to_owned(),
        callback_url: "http://localhost/callback?code=test&state=test".to_owned(),
    };

    services
        .openai()
        .start_authorization(StartAuthorization {
            context: context("oauth-store-retry"),
            name: "new OpenAI credential".to_owned(),
            reauthorization: None,
        })
        .await
        .expect("start authorization");
    services
        .openai()
        .complete_authorization(command())
        .await
        .expect_err("Store commit must fail");
    services
        .openai()
        .complete_authorization(command())
        .await
        .expect("released claim must permit retry");

    assert_eq!(
        recorded(&events),
        [
            "provider.start_authorization",
            "provider.complete_authorization",
            "store.commit_authorization",
            "authorization_guard.abort",
            "provider.complete_authorization",
            "store.commit_authorization",
            "authorization_guard.commit",
            "provider.quota",
        ]
    );
    assert!(provider.pending().is_none());
}

#[tokio::test]
async fn openai_import_provider_error_should_not_touch_store_transaction() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    provider.fail_next(ProviderAdminErrorKind::Invalid);
    let store = FakeAccountStore::new("openai", events.clone());
    let services = service(provider, store).await;

    services
        .openai()
        .import_document(ImportCredentials {
            context: context("import-openai-error"),
            document: document(),
        })
        .await
        .expect_err("invalid Provider document");

    assert_eq!(recorded(&events), ["provider.prepare_import"]);
}

#[tokio::test]
async fn openai_reauthorization_should_commit_after_credential_revision_advances() {
    let events = events();
    let provider = FakeProviderAdmin::new("openai", events.clone());
    let store = FakeAccountStore::new("openai", events.clone());
    let services = service(provider.clone(), store.clone()).await;
    services
        .openai()
        .start_authorization(StartAuthorization {
            context: context("oauth-start-openai"),
            name: "reauthorize".to_owned(),
            reauthorization: Some(ProviderAccountId::new("acct_test").expect("account ID")),
        })
        .await
        .expect("start reauthorization");

    let pending = provider.pending().expect("pending envelope");
    assert_eq!(pending.provider_kind().as_str(), "openai");
    assert_eq!(
        pending.owner_binding().started_request_id(),
        "oauth-start-openai"
    );
    assert!(matches!(
        pending.target(),
        AuthorizationMutationTarget::Reauthorize { account_id }
            if account_id.as_str() == "acct_test"
    ));
    assert!(!format!("{pending:?}").contains("admin-test"));

    provider.set_current_credential_revision(revision(2));

    let result = services
        .openai()
        .complete_authorization(CompleteAuthorization {
            context: context("oauth-complete-openai"),
            flow_id: "flow-test".to_owned(),
            callback_url: "http://localhost/callback?code=test&state=test".to_owned(),
        })
        .await
        .expect("complete reauthorization");

    assert_eq!(result.credential_revision, Some(revision(3)));

    assert_eq!(
        recorded(&events),
        [
            "store.credential_details",
            "provider.start_authorization",
            "provider.complete_authorization",
            "store.commit_authorization",
            "guard.finish",
        ]
    );
    assert_eq!(store.audit_requests(), ["oauth-complete-openai"]);
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
        context: context("request-openai"),
        account_id: ProviderAccountId::new(account_id).expect("account ID"),
    }
}

fn deletion(account_id: &str) -> CredentialDeletion {
    CredentialDeletion {
        context: context("request-openai"),
        account_ids: vec![ProviderAccountId::new(account_id).expect("account ID")],
    }
}
