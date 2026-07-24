use std::sync::Arc;

use chrono::Utc;
use gateway_core::engine::credential::{
    AccountAvailability, CredentialCasOutcome, CredentialRevision, NewProviderAccount,
    PlaintextCredential, ProviderAccount, ProviderAccountStore,
};
use gateway_core::error::StoreErrorKind;
use gateway_core::routing::ProviderKind;
use provider_xai::{
    GrokCredentialAdmin, GrokCredentialAvailability, GrokCredentialRepository,
    GrokCredentialRepositoryError, GrokOAuthSecret, RotateManagedGrokCredential, SecretValue,
    UpdateGrokCredentialState,
};

use crate::support::{
    MemoryProviderAccountStore, account_id, create_input, credential_object, prepare_input,
    profile, seed_input,
};

fn repository() -> (Arc<MemoryProviderAccountStore>, GrokCredentialRepository) {
    let store = MemoryProviderAccountStore::shared();
    let account_store: Arc<dyn ProviderAccountStore> = store.clone();
    (store, GrokCredentialRepository::new(account_store))
}

#[tokio::test]
async fn create_persists_plaintext_oauth_json_without_envelope_fields() {
    let (store, _) = repository();
    let input = create_input("plain", "subject-plain");
    seed_input(&store, &input).await.expect("create account");

    let credential = store
        .credential(&input.account_id)
        .expect("stored credential");
    let object = credential_object(&credential);
    assert_eq!(
        object.get("auth_method").and_then(|v| v.as_str()),
        Some("oauth")
    );
    assert_eq!(
        object.get("access_token").and_then(|v| v.as_str()),
        Some("access-plain")
    );
    assert_eq!(
        object.get("refresh_token").and_then(|v| v.as_str()),
        Some("refresh-plain")
    );
    assert!(!object.contains_key("secret_envelope"));
    assert!(!object.contains_key("secret_key_id"));
}

#[tokio::test]
async fn create_projects_identity_and_revision_to_common_columns() {
    let (store, _) = repository();
    let input = create_input("projection", "subject-projection");
    let prepared = prepare_input(&input).expect("prepare account");
    assert_eq!(prepared.account.id(), &input.account_id);
    assert_eq!(prepared.account.revision().get(), 1);
    store
        .create_account(prepared)
        .await
        .expect("create account");
    let account = store.account(&input.account_id).expect("account");

    assert_eq!(account.upstream_user_id(), "subject-projection");
    assert_eq!(account.email(), Some("subject-projection@example.com"));
    assert!(account.has_refresh_token());
}

#[tokio::test]
async fn duplicate_account_is_rejected_by_store_contract() {
    let (store, _) = repository();
    let input = create_input("duplicate", "subject-duplicate");
    seed_input(&store, &input).await.expect("first create");
    let error = seed_input(&store, &input).await.expect_err("duplicate");
    assert_eq!(error.kind(), StoreErrorKind::Conflict);
}

#[tokio::test]
async fn rotate_uses_revision_cas_and_replaces_plaintext_tokens() {
    let (store, _) = repository();
    let input = create_input("rotate", "subject-rotate");
    seed_input(&store, &input).await.expect("create");
    let current = store
        .load_credential(
            &input.account_id,
            CredentialRevision::new(1).expect("revision"),
        )
        .await
        .expect("current credential");
    let prepared = GrokCredentialAdmin
        .prepare_rotation(&RotateManagedGrokCredential {
            current,
            secret: GrokOAuthSecret {
                access_token: SecretValue::new("access-rotated"),
                refresh_token: SecretValue::new("refresh-rotated"),
                id_token: None,
                scope: provider_xai::OFFICIAL_SCOPES.join(" "),
            },
            verified_account: profile("subject-rotate"),
            next_refresh_at: chrono::Utc::now() + chrono::Duration::minutes(30),
        })
        .expect("rotate");
    assert!(matches!(
        store
            .compare_and_swap_credential(prepared.credential)
            .await
            .expect("persist rotation"),
        CredentialCasOutcome::Updated(revision) if revision.get() == 2
    ));
    let credential = store.credential(&input.account_id).expect("credential");
    assert_eq!(
        credential_object(&credential)
            .get("access_token")
            .and_then(|value| value.as_str()),
        Some("access-rotated")
    );
    assert_eq!(
        credential_object(&credential)
            .get("id_token")
            .and_then(|value| value.as_str()),
        Some("id-rotate")
    );
}

#[tokio::test]
async fn stale_rotate_does_not_modify_tokens() {
    let (store, _) = repository();
    let input = create_input("stale", "subject-stale");
    seed_input(&store, &input).await.expect("create");
    let current = store
        .load_credential(
            &input.account_id,
            CredentialRevision::new(1).expect("revision"),
        )
        .await
        .expect("current credential");
    let winning = GrokCredentialAdmin
        .prepare_rotation(&RotateManagedGrokCredential {
            current: current.clone(),
            secret: GrokOAuthSecret {
                access_token: SecretValue::new("winning-access"),
                refresh_token: SecretValue::new("winning-refresh"),
                id_token: None,
                scope: provider_xai::OFFICIAL_SCOPES.join(" "),
            },
            verified_account: profile("subject-stale"),
            next_refresh_at: chrono::Utc::now() + chrono::Duration::minutes(30),
        })
        .expect("winning rotation");
    let stale = GrokCredentialAdmin
        .prepare_rotation(&RotateManagedGrokCredential {
            current,
            secret: GrokOAuthSecret {
                access_token: SecretValue::new("wrong"),
                refresh_token: SecretValue::new("wrong"),
                id_token: None,
                scope: provider_xai::OFFICIAL_SCOPES.join(" "),
            },
            verified_account: profile("subject-stale"),
            next_refresh_at: chrono::Utc::now() + chrono::Duration::minutes(30),
        })
        .expect("stale command");
    assert!(matches!(
        store
            .compare_and_swap_credential(winning.credential)
            .await
            .expect("winning write"),
        CredentialCasOutcome::Updated(_)
    ));
    assert_eq!(
        store
            .compare_and_swap_credential(stale.credential)
            .await
            .expect("stale write"),
        CredentialCasOutcome::Conflict
    );
    let credential = store.credential(&input.account_id).expect("credential");
    assert_eq!(
        credential_object(&credential)
            .get("access_token")
            .and_then(|value| value.as_str()),
        Some("winning-access")
    );
}

#[tokio::test]
async fn rotate_rejects_verified_identity_rebind() {
    let (store, _) = repository();
    let input = create_input("identity", "subject-a");
    seed_input(&store, &input).await.expect("create");
    let current = store
        .load_credential(
            &input.account_id,
            CredentialRevision::new(1).expect("revision"),
        )
        .await
        .expect("current credential");
    let result = GrokCredentialAdmin.prepare_rotation(&RotateManagedGrokCredential {
        current,
        secret: GrokOAuthSecret {
            access_token: SecretValue::new("new-access"),
            refresh_token: SecretValue::new("new-refresh"),
            id_token: None,
            scope: provider_xai::OFFICIAL_SCOPES.join(" "),
        },
        verified_account: profile("subject-b"),
        next_refresh_at: chrono::Utc::now() + chrono::Duration::minutes(30),
    });
    assert!(matches!(
        result,
        Err(GrokCredentialRepositoryError::IdentityRebind)
    ));
}

#[tokio::test]
async fn state_update_uses_credential_revision_fence() {
    let (store, repository) = repository();
    let input = create_input("state", "subject-state");
    seed_input(&store, &input).await.expect("create");
    let cooldown_until = Utc::now() + chrono::Duration::minutes(1);
    repository
        .update_state(&UpdateGrokCredentialState {
            account_id: input.account_id.clone(),
            expected_revision: CredentialRevision::new(1).expect("revision"),
            availability: GrokCredentialAvailability::Cooldown,
            availability_reason: Some("rate_limited".to_owned()),
            cooldown_until: Some(cooldown_until),
            observed_at: Utc::now(),
        })
        .await
        .expect("state update");
    let account = store.account(&input.account_id).expect("account");
    assert_eq!(account.availability(), AccountAvailability::Cooldown);
    assert!(account.cooldown_until().is_some());
}

#[tokio::test]
async fn cooldown_requires_matching_deadline() {
    let (store, repository) = repository();
    let input = create_input("bad-cooldown", "subject-cooldown");
    seed_input(&store, &input).await.expect("create");
    let result = repository
        .update_state(&UpdateGrokCredentialState {
            account_id: input.account_id,
            expected_revision: CredentialRevision::new(1).expect("revision"),
            availability: GrokCredentialAvailability::Cooldown,
            availability_reason: None,
            cooldown_until: None,
            observed_at: Utc::now(),
        })
        .await;
    assert_eq!(
        result,
        Err(GrokCredentialRepositoryError::InvalidInput(
            "cooldown_until"
        ))
    );
}

#[tokio::test]
async fn admin_prepare_does_not_mutate_provider_account_store() {
    let (store, _) = repository();
    let input = create_input("admin", "subject-admin");
    let prepared = GrokCredentialAdmin
        .prepare_import(&input)
        .expect("prepare import");
    assert_eq!(prepared.account.id(), &input.account_id);
    assert!(store.account(&input.account_id).is_none());
}

#[tokio::test]
async fn repository_rejects_account_owned_by_another_provider() {
    let (store, _) = repository();
    let id = account_id("codex-owned");
    let revision = CredentialRevision::new(1).expect("revision");
    let account = ProviderAccount::new(
        id.clone(),
        ProviderKind::new("openai").expect("provider"),
        "other".to_owned(),
        "subject".to_owned(),
        "oauth".to_owned(),
        revision,
        Some(std::time::SystemTime::now() + std::time::Duration::from_secs(3600)),
    );
    let mut object = serde_json::Map::new();
    object.insert("access_token".to_owned(), serde_json::json!("secret"));
    store
        .create_account(NewProviderAccount {
            account,
            credential: PlaintextCredential::new(object),
        })
        .await
        .expect("seed other provider");
    let current = store
        .load_credential(&id, revision)
        .await
        .expect("load other provider");
    assert!(matches!(
        GrokCredentialAdmin.prepare_rotation(&RotateManagedGrokCredential {
            current,
            secret: GrokOAuthSecret {
                access_token: SecretValue::new("access"),
                refresh_token: SecretValue::new("refresh"),
                id_token: None,
                scope: provider_xai::OFFICIAL_SCOPES.join(" "),
            },
            verified_account: profile("subject"),
            next_refresh_at: chrono::Utc::now() + chrono::Duration::minutes(30),
        }),
        Err(GrokCredentialRepositoryError::WrongProviderKind)
    ));
}

#[tokio::test]
async fn invalid_token_lifetime_is_rejected_before_store_write() {
    let (store, _) = repository();
    let mut input = create_input("lifetime", "subject-lifetime");
    input.account.refresh_token_expires_at = Some(input.account.access_token_expires_at);
    assert_eq!(
        GrokCredentialAdmin.prepare_import(&input),
        Err(GrokCredentialRepositoryError::InvalidInput(
            "token_lifetime"
        ))
    );
    assert_eq!(store.len(), 0);
}

#[tokio::test]
async fn unknown_refresh_token_expiry_is_valid_and_not_invented_in_secret_json() {
    let (store, _) = repository();
    let mut input = create_input("unknown-rt-expiry", "subject-unknown-rt-expiry");
    input.account.refresh_token_expires_at = None;

    seed_input(&store, &input).await.expect("create account");
    let credential = store.credential(&input.account_id).expect("credential");
    assert!(
        !credential_object(&credential).contains_key("refresh_token_expires_at"),
        "unknown Provider fact must remain absent"
    );
}

#[tokio::test]
async fn oauth_bundle_export_is_provider_owned_canonical_and_debug_redacted() {
    let (store, _) = repository();
    let mut input = create_input("export", "subject-export");
    input.secret.scope =
        "openid profile email offline_access grok-cli:access api:access".to_owned();
    seed_input(&store, &input).await.expect("create account");
    let loaded = store
        .load_credential(
            &input.account_id,
            CredentialRevision::new(1).expect("revision"),
        )
        .await
        .expect("loaded credential");

    let export = GrokCredentialAdmin
        .export_oauth_bundle(&[loaded], Utc::now())
        .expect("export");
    let debug = format!("{export:?}");
    assert!(debug.contains("REDACTED"));
    for secret in ["access-export", "refresh-export", "id-export"] {
        assert!(!debug.contains(secret));
    }
    let value = export.into_value();
    assert_eq!(value["version"], 1);
    assert_eq!(value["type"], "oauth-account-bundle");
    assert_eq!(value["accounts"][0]["platform"], "grok");
    assert_eq!(value["accounts"][0]["type"], "oauth");
    assert_eq!(
        value["accounts"][0]["credentials"]["base_url"],
        provider_xai::GROK_CLI_BASE_URL
    );
    assert_eq!(
        value["accounts"][0]["credentials"]["access_token"],
        "access-export"
    );
    assert_eq!(
        value["accounts"][0]["credentials"]["scope"],
        "openid profile email offline_access grok-cli:access api:access"
    );
    assert_eq!(value["proxies"], serde_json::json!([]));
}

#[test]
fn oauth_secret_debug_never_exposes_plaintext() {
    let secret = GrokOAuthSecret {
        access_token: SecretValue::new("access-visible-only-to-provider"),
        refresh_token: SecretValue::new("refresh-visible-only-to-provider"),
        id_token: Some(SecretValue::new("id-visible-only-to-provider")),
        scope: "scope-visible-only-to-provider".to_owned(),
    };
    let debug = format!("{secret:?}");
    assert!(debug.contains("REDACTED"));
    assert!(!debug.contains("visible-only-to-provider"));
}
