use std::sync::Arc;

use chrono::{TimeZone as _, Utc};
use futures::executor::block_on;
use gateway_core::engine::credential::ProviderAccountId;
use provider_openai::credential::{CodexCredentialCatalogError, CodexCredentialCatalogService};
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};

use crate::support::{MemoryAccountStore, agent_identity_service};

fn wire_profile() -> CodexWireProfileState {
    CodexWireProfileState::new(CodexWireProfile {
        originator: "codex_cli_rs".to_owned(),
        codex_version: "0.144.0".to_owned(),
        desktop_version: "1.0.0".to_owned(),
        desktop_build: "1".to_owned(),
        os_type: "linux".to_owned(),
        os_version: "6.8".to_owned(),
        arch: "x86_64".to_owned(),
        terminal: "catalog-contract".to_owned(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 7, 18, 0, 0, 0)
            .single()
            .expect("fixture time"),
    })
}

fn service(store: &Arc<MemoryAccountStore>) -> CodexCredentialCatalogService {
    CodexCredentialCatalogService::new(
        store.repository(),
        wire_profile(),
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        agent_identity_service(store),
    )
}

#[test]
fn catalog_starts_without_a_snapshot_or_generation() {
    let store = Arc::new(MemoryAccountStore::default());
    let service = service(&store);

    assert!(service.cached().expect("cache read").is_none());
    assert_eq!(service.catalog_generation().get(), 0);
}

#[test]
fn catalog_without_openai_accounts_fails_before_network_io() {
    let store = Arc::new(MemoryAccountStore::default());

    let error = block_on(service(&store).synchronize()).expect_err("empty account set");

    assert!(matches!(
        error,
        CodexCredentialCatalogError::NoEligibleCredential
    ));
}

#[tokio::test]
async fn missing_account_refresh_fails_before_network_io() {
    let store = Arc::new(MemoryAccountStore::default());
    let account = ProviderAccountId::new("acct_missing").expect("account id");

    let error = service(&store)
        .synchronize_account(&account)
        .await
        .expect_err("missing account");

    assert!(matches!(
        error,
        CodexCredentialCatalogError::NoEligibleCredential
    ));
}

#[tokio::test]
async fn response_etag_change_is_deduplicated_and_queued_once() {
    let store = Arc::new(MemoryAccountStore::default());
    let service = service(&store);

    assert!(
        service
            .observe_response_etag("\"models-v2\"")
            .expect("new ETag")
    );
    assert!(
        !service
            .observe_response_etag("\"models-v2\"")
            .expect("duplicate ETag")
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        service.wait_for_etag_refresh(),
    )
    .await
    .expect("queued ETag refresh");
}

#[test]
fn invalid_response_etag_is_rejected_without_touching_the_catalog() {
    let store = Arc::new(MemoryAccountStore::default());
    let service = service(&store);

    let error = service
        .observe_response_etag("invalid\netag")
        .expect_err("invalid ETag");

    assert!(matches!(error, CodexCredentialCatalogError::InvalidEtag));
    assert!(service.cached().expect("cache read").is_none());
    assert_eq!(service.catalog_generation().get(), 0);
}
