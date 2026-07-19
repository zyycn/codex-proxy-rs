use std::sync::Arc;

use gateway_core::engine::credential::ProviderAccountStore;
use provider_xai::{GrokCredentialAdmin, GrokCredentialRepository, GrokCredentialRepositoryError};

use crate::support::{MemoryProviderAccountStore, create_input, seed_input};

#[test]
fn repository_rejects_identity_that_cannot_be_sent_as_official_header() {
    let mut input = create_input("invalid", "subject");
    input.account.subject = "subject-with-非-ascii".to_owned();

    assert_eq!(
        GrokCredentialAdmin.prepare_import(&input),
        Err(GrokCredentialRepositoryError::InvalidInput("subject"))
    );
}

#[tokio::test]
async fn lifecycle_projection_should_return_expiry_without_oauth_secrets() {
    let store = MemoryProviderAccountStore::shared();
    let account_store: Arc<dyn ProviderAccountStore> = store.clone();
    let repository = GrokCredentialRepository::new(account_store);
    let input = create_input("lifecycle", "subject-lifecycle");
    let expected_expiry = input.account.refresh_token_expires_at;
    seed_input(&store, &input).await.expect("seed account");

    let lifecycle = repository
        .read_lifecycle(&input.account_id)
        .await
        .expect("read lifecycle");

    assert_eq!(lifecycle.account_id(), &input.account_id);
    assert_eq!(lifecycle.credential_revision().get(), 1);
    assert_eq!(
        lifecycle.refresh_token_expires_at().copied(),
        expected_expiry
    );
    let debug = format!("{lifecycle:?}");
    assert!(!debug.contains("access-lifecycle"));
    assert!(!debug.contains("refresh-lifecycle"));
    assert!(!debug.contains("id-lifecycle"));
}
