use chrono::Utc;
use gateway_store::{
    Revision,
    redis::{
        CredentialStateCache, ProviderAccountCatalogCacheKey,
        ProviderAccountCatalogCacheRepository, RedisCredentialStateRepository,
    },
};

#[test]
fn credential_state_adapter_implements_opaque_catalog_cache_port() {
    fn assert_port<T: ProviderAccountCatalogCacheRepository>() {}
    assert_port::<RedisCredentialStateRepository>();
}

#[test]
fn credential_state_rejects_provider_specific_status() {
    let state = CredentialStateCache {
        provider_account_id: "account-1".to_owned(),
        revision: Revision::new(1).expect("positive revision"),
        enabled: true,
        availability: "codex_special".to_owned(),
        observed_at: Utc::now(),
    };
    assert!(state.validate().is_err());
}

#[test]
fn provider_catalog_cache_key_requires_provider_and_account() {
    let key = ProviderAccountCatalogCacheKey {
        provider_kind: "xai".to_owned(),
        provider_account_id: String::new(),
        credential_revision: Revision::new(1).expect("positive revision"),
    };
    assert!(key.validate().is_err());
}
