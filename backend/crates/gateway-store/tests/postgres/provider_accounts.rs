use std::time::{Duration, SystemTime};

use chrono::{TimeDelta, Utc};
use gateway_core::engine::credential::{
    CredentialCasOutcome, CredentialCasUpdate, CredentialRevision, PlaintextCredential,
    ProviderAccountId, ProviderAccountStore, ProviderAccountUpdate,
};
use gateway_store::{
    ConflictKind, JsonObject, Revision, StoreError,
    postgres::{
        AdminAuditActorKind, AdminAuditEvent, DeleteProviderAccounts, ImportProviderAccounts,
        NewProviderAccount, PgProviderAccountRepository, ProviderAccountAdminRepository,
        ProviderAccountAdminScope, ProviderAccountAvailability, ProviderAccountObservation,
        ProviderAccountRepository, ProviderCredentialUpdate, RotateProviderAccount,
        SetProviderAccountEnabled, UpdateProviderAccount,
    },
};
use serde_json::json;

use super::TestDatabase;

#[test]
fn postgres_provider_account_adapter_implements_core_port() {
    fn assert_port<T: ProviderAccountStore>() {}
    assert_port::<PgProviderAccountRepository>();

    fn assert_admin_port<T: ProviderAccountAdminRepository>() {}
    assert_admin_port::<PgProviderAccountRepository>();
}

#[tokio::test]
async fn admin_import_updates_the_same_verified_identity_without_rebinding_it() {
    let Some(database) = TestDatabase::create("provider_account_admin_upsert").await else {
        return;
    };
    seed_instance(&database.pool, "inst_openai_admin", "openai")
        .await
        .expect("seed Codex instance");
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let scope = ProviderAccountAdminScope {
        provider_kind: "openai".to_owned(),
        provider_instance_id: "inst_openai_admin".to_owned(),
    };
    let revision = repository
        .import_provider_accounts(
            Revision::new(1).expect("initial revision"),
            ImportProviderAccounts {
                scope: scope.clone(),
                accounts: vec![account("acct_admin_upsert", "user-admin-upsert")],
                audit: audit("audit_admin_upsert_create", "import", "acct_admin_upsert"),
            },
        )
        .await
        .expect("create imported account");
    sqlx::query(
        "update provider_accounts
         set provider_quota_json = '{}'::jsonb, quota_observed_at = now(), updated_at = now()
         where id = 'acct_admin_upsert'",
    )
    .execute(&database.pool)
    .await
    .expect("seed stale quota");

    let mut updated = account("acct_admin_upsert", "user-admin-upsert");
    updated.name = "updated import".to_owned();
    updated.provider_credentials_json = credential_json("updated-import-secret");
    let revision = repository
        .import_provider_accounts(
            revision,
            ImportProviderAccounts {
                scope: scope.clone(),
                accounts: vec![updated],
                audit: audit("audit_admin_upsert_update", "import", "acct_admin_upsert"),
            },
        )
        .await
        .expect("update the same imported identity");
    assert_eq!(revision.get(), 3);
    let row: (String, serde_json::Value, i64, Option<serde_json::Value>) = sqlx::query_as(
        "select name, provider_credentials_json, credential_revision, provider_quota_json
         from provider_accounts where id = 'acct_admin_upsert'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load updated import");
    assert_eq!(row.0, "updated import");
    assert_eq!(row.1["access_token"], "updated-import-secret");
    assert_eq!(row.2, 2);
    assert!(
        row.3.is_none(),
        "credential replacement must clear stale quota"
    );

    repository
        .import_provider_accounts(
            revision,
            ImportProviderAccounts {
                scope,
                accounts: vec![account("acct_admin_upsert", "another-user")],
                audit: audit("audit_admin_upsert_rebind", "import", "acct_admin_upsert"),
            },
        )
        .await
        .expect_err("an existing account ID must not be rebound");
    assert_eq!(current_revision(&database.pool).await, 3);

    database.close().await;
}

#[tokio::test]
async fn core_refresh_cas_updates_profile_and_credential_under_one_revision() {
    let Some(database) = TestDatabase::create("provider_account_core_refresh").await else {
        return;
    };
    seed_instance(&database.pool, "inst_xai_refresh", "xai")
        .await
        .expect("seed xAI instance");
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    repository
        .insert_provider_account(NewProviderAccount {
            id: "acct_core_refresh".to_owned(),
            provider_instance_id: "inst_xai_refresh".to_owned(),
            provider_kind: "xai".to_owned(),
            name: "before refresh".to_owned(),
            email: Some("before@example.invalid".to_owned()),
            upstream_user_id: "upstream-core-refresh".to_owned(),
            upstream_account_id: None,
            plan_type: Some("free".to_owned()),
            provider_credentials_json: credential_json("before-secret"),
            has_refresh_token: false,
            access_token_expires_at: Utc::now() + TimeDelta::minutes(5),
            next_refresh_at: None,
            enabled: true,
            availability: ProviderAccountAvailability::Ready,
            cooldown_until: None,
            availability_observed_at: Utc::now(),
        })
        .await
        .expect("seed provider account");

    let account_id = ProviderAccountId::new("acct_core_refresh").expect("account ID");
    let refreshed = CredentialCasUpdate::new(
        account_id.clone(),
        CredentialRevision::new(1).expect("credential revision"),
        ProviderAccountUpdate {
            account_id: account_id.clone(),
            name: "after refresh".to_owned(),
            email: Some("after@example.invalid".to_owned()),
            plan_type: Some("premium".to_owned()),
        },
        plaintext_credential("after-secret"),
        true,
        SystemTime::now() + Duration::from_secs(3_600),
        Some(SystemTime::now() + Duration::from_secs(1_800)),
    )
    .expect("valid refresh update");
    assert_eq!(
        repository
            .compare_and_swap_credential(refreshed)
            .await
            .expect("refresh credential"),
        CredentialCasOutcome::Updated(
            CredentialRevision::new(2).expect("updated credential revision")
        )
    );

    let row: (
        String,
        Option<String>,
        Option<String>,
        serde_json::Value,
        i64,
    ) = sqlx::query_as(
        "select name, email, plan_type, provider_credentials_json, credential_revision
         from provider_accounts where id = 'acct_core_refresh'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load refreshed account");
    assert_eq!(row.0, "after refresh");
    assert_eq!(row.1.as_deref(), Some("after@example.invalid"));
    assert_eq!(row.2.as_deref(), Some("premium"));
    assert_eq!(row.3["access_token"], "after-secret");
    assert_eq!(row.4, 2);

    let stale = CredentialCasUpdate::new(
        account_id.clone(),
        CredentialRevision::new(1).expect("stale credential revision"),
        ProviderAccountUpdate {
            account_id,
            name: "must not persist".to_owned(),
            email: None,
            plan_type: None,
        },
        plaintext_credential("must-not-persist"),
        false,
        SystemTime::now() + Duration::from_secs(3_600),
        None,
    )
    .expect("valid stale update");
    assert_eq!(
        repository
            .compare_and_swap_credential(stale)
            .await
            .expect("stale CAS is an outcome"),
        CredentialCasOutcome::Conflict
    );
    let unchanged: (String, serde_json::Value, i64) = sqlx::query_as(
        "select name, provider_credentials_json, credential_revision
         from provider_accounts where id = 'acct_core_refresh'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load account after stale CAS");
    assert_eq!(unchanged.0, "after refresh");
    assert_eq!(unchanged.1["access_token"], "after-secret");
    assert_eq!(unchanged.2, 2);

    database.close().await;
}

#[tokio::test]
async fn provider_account_admin_mutations_are_scoped_audited_and_atomic() {
    let Some(database) = TestDatabase::create("provider_account_admin").await else {
        return;
    };
    seed_instance(&database.pool, "inst_openai_admin", "openai")
        .await
        .expect("seed Codex instance");
    seed_instance(&database.pool, "inst_xai_admin", "xai")
        .await
        .expect("seed xAI instance");
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let scope = ProviderAccountAdminScope {
        provider_kind: "openai".to_owned(),
        provider_instance_id: "inst_openai_admin".to_owned(),
    };

    let mut cooldown_account = account("acct_admin_b", "user-admin-b");
    cooldown_account.availability = ProviderAccountAvailability::Cooldown;
    cooldown_account.cooldown_until = Some(Utc::now() + TimeDelta::minutes(10));
    let revision = repository
        .import_provider_accounts(
            Revision::new(1).expect("initial revision"),
            ImportProviderAccounts {
                scope: scope.clone(),
                accounts: vec![account("acct_admin_a", "user-admin-a"), cooldown_account],
                audit: audit("audit_account_batch", "import_batch", "provider_accounts"),
            },
        )
        .await
        .expect("import provider accounts");
    assert_eq!(revision.get(), 2);
    let ready: (bool, String, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
        "select enabled, availability, cooldown_until from provider_accounts where id = $1",
    )
    .bind("acct_admin_a")
    .fetch_one(&database.pool)
    .await
    .expect("load ready imported account");
    assert_eq!(ready, (true, "ready".to_owned(), None));
    let cooldown: (bool, String, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
        "select enabled, availability, cooldown_until from provider_accounts where id = $1",
    )
    .bind("acct_admin_b")
    .fetch_one(&database.pool)
    .await
    .expect("load cooldown imported account");
    assert!(cooldown.0);
    assert_eq!(cooldown.1, "cooldown");
    assert!(cooldown.2.is_some());

    repository
        .import_provider_accounts(
            revision,
            ImportProviderAccounts {
                scope: scope.clone(),
                accounts: vec![
                    account("acct_admin_transient", "user-admin-transient"),
                    account("acct_admin_a", "user-admin-duplicate"),
                ],
                audit: audit(
                    "audit_account_failed_batch",
                    "import_batch",
                    "provider_accounts",
                ),
            },
        )
        .await
        .expect_err("duplicate batch row must roll back the entire import");
    assert_eq!(current_revision(&database.pool).await, 2);
    assert_eq!(
        account_count(&database.pool, "acct_admin_transient").await,
        0
    );

    let wrong_scope = ProviderAccountAdminScope {
        provider_kind: "xai".to_owned(),
        provider_instance_id: "inst_xai_admin".to_owned(),
    };
    let wrong_scope_error = repository
        .rotate_provider_account(
            revision,
            RotateProviderAccount {
                scope: wrong_scope,
                profile: profile("acct_admin_a", "wrong scope"),
                credential: credential_update("acct_admin_a", 1, "wrong-scope-secret"),
                audit: audit("audit_wrong_scope", "rotate", "acct_admin_a"),
            },
        )
        .await
        .expect_err("Provider endpoint must not rotate another Provider account");
    assert!(matches!(
        wrong_scope_error,
        StoreError::Conflict {
            kind: ConflictKind::StaleRevision,
            ..
        }
    ));
    assert_eq!(current_revision(&database.pool).await, 2);

    let rotation = repository
        .rotate_provider_account(
            revision,
            RotateProviderAccount {
                scope: scope.clone(),
                profile: profile("acct_admin_a", "rotated account"),
                credential: credential_update("acct_admin_a", 1, "rotated-secret"),
                audit: audit("audit_account_rotate", "rotate", "acct_admin_a"),
            },
        )
        .await
        .expect("rotate provider account");
    assert_eq!(rotation.config_revision.get(), 3);
    assert_eq!(rotation.credential_revision.get(), 2);

    let revision = repository
        .set_provider_account_enabled_admin(
            rotation.config_revision,
            SetProviderAccountEnabled {
                scope: scope.clone(),
                account_id: "acct_admin_a".to_owned(),
                enabled: false,
                audit: audit("audit_account_disable", "disable", "acct_admin_a"),
            },
        )
        .await
        .expect("disable provider account");
    repository
        .delete_provider_accounts_admin(
            revision,
            DeleteProviderAccounts {
                scope: scope.clone(),
                account_ids: vec!["acct_admin_a".to_owned(), "acct_admin_b".to_owned()],
                audit: audit("audit_partial_delete", "delete", "provider_accounts"),
            },
        )
        .await
        .expect_err("mixed enabled state batch must roll back every deletion");
    assert_eq!(current_revision(&database.pool).await, 4);
    assert_eq!(account_count(&database.pool, "acct_admin_a").await, 1);
    assert_eq!(account_count(&database.pool, "acct_admin_b").await, 1);

    let exports = repository
        .export_provider_accounts(
            scope.clone(),
            vec!["acct_admin_b".to_owned(), "acct_admin_a".to_owned()],
        )
        .await
        .expect("export selected provider accounts");
    assert_eq!(exports[0].summary.id, "acct_admin_b");
    assert_eq!(exports[1].summary.id, "acct_admin_a");
    assert!(!format!("{exports:?}").contains("rotated-secret"));

    let revision = repository
        .delete_provider_accounts_admin(
            revision,
            DeleteProviderAccounts {
                scope: scope.clone(),
                account_ids: vec!["acct_admin_a".to_owned()],
                audit: audit("audit_account_delete", "delete", "acct_admin_a"),
            },
        )
        .await
        .expect("delete disabled provider account");
    assert_eq!(revision.get(), 5);

    repository
        .delete_provider_accounts_admin(
            revision,
            DeleteProviderAccounts {
                scope,
                account_ids: vec!["acct_admin_b".to_owned()],
                audit: audit("audit_enabled_delete", "delete", "acct_admin_b"),
            },
        )
        .await
        .expect_err("enabled provider account must not be deleted");
    assert_eq!(current_revision(&database.pool).await, 5);
    assert_eq!(account_count(&database.pool, "acct_admin_b").await, 1);
    let audit_count: i64 = sqlx::query_scalar("select count(*) from admin_audit_events")
        .fetch_one(&database.pool)
        .await
        .expect("count provider account audits");
    assert_eq!(audit_count, 4);

    database.close().await;
}

fn account(id: &str, upstream_user_id: &str) -> NewProviderAccount {
    NewProviderAccount {
        id: id.to_owned(),
        provider_instance_id: "inst_openai_admin".to_owned(),
        provider_kind: "openai".to_owned(),
        name: id.to_owned(),
        email: Some(format!("{id}@example.invalid")),
        upstream_user_id: upstream_user_id.to_owned(),
        upstream_account_id: None,
        plan_type: Some("pro".to_owned()),
        provider_credentials_json: credential_json("initial-secret"),
        has_refresh_token: false,
        access_token_expires_at: Utc::now() + TimeDelta::hours(1),
        next_refresh_at: None,
        enabled: true,
        availability: ProviderAccountAvailability::Ready,
        cooldown_until: None,
        availability_observed_at: Utc::now(),
    }
}

fn credential_update(account_id: &str, revision: u64, marker: &str) -> ProviderCredentialUpdate {
    ProviderCredentialUpdate {
        account_id: account_id.to_owned(),
        expected_revision: Revision::new(revision).expect("credential revision"),
        provider_credentials_json: credential_json(marker),
        has_refresh_token: false,
        access_token_expires_at: Utc::now() + TimeDelta::hours(2),
        next_refresh_at: None,
    }
}

fn profile(account_id: &str, name: &str) -> UpdateProviderAccount {
    UpdateProviderAccount {
        id: account_id.to_owned(),
        name: name.to_owned(),
        email: Some(format!("{account_id}@rotated.example.invalid")),
        plan_type: Some("team".to_owned()),
    }
}

fn credential_json(marker: &str) -> JsonObject {
    JsonObject::try_from_value(
        "provider_credentials_json",
        json!({ "access_token": marker }),
        256 * 1024,
    )
    .expect("credential JSON")
}

fn plaintext_credential(marker: &str) -> PlaintextCredential {
    PlaintextCredential::new(
        [("access_token".to_owned(), json!(marker))]
            .into_iter()
            .collect(),
    )
}

fn audit(id: &str, action: &str, entity_ref: &str) -> AdminAuditEvent {
    AdminAuditEvent {
        id: id.to_owned(),
        actor_kind: AdminAuditActorKind::System,
        actor_admin_user_id: None,
        actor_ref: "system:provider-test".to_owned(),
        admin_request_id: Some(format!("request:{id}")),
        action: action.to_owned(),
        entity_kind: "provider_account".to_owned(),
        entity_ref: entity_ref.to_owned(),
        config_revision: None,
        changed_fields: vec!["provider_account".to_owned()],
        created_at: Utc::now(),
    }
}

async fn seed_instance(
    pool: &sqlx::PgPool,
    id: &str,
    provider_kind: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into provider_instances (
           id, provider_kind, name, base_url, enabled, created_at, updated_at
         ) values ($1, $2, $1, 'https://example.invalid', true, now(), now())",
    )
    .bind(id)
    .bind(provider_kind)
    .execute(pool)
    .await?;
    Ok(())
}

async fn current_revision(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("select config_revision from runtime_settings where id = 1")
        .fetch_one(pool)
        .await
        .expect("load config revision")
}

async fn account_count(pool: &sqlx::PgPool, account_id: &str) -> i64 {
    sqlx::query_scalar("select count(*) from provider_accounts where id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await
        .expect("count provider account")
}

#[test]
fn provider_credentials_are_redacted_from_debug() {
    let secret = "secret-access-token";
    let object =
        JsonObject::try_from_value("credentials", json!({ "access_token": secret }), 256 * 1024)
            .expect("object is valid");
    assert!(!format!("{object:?}").contains(secret));
}

#[test]
fn cooldown_observation_requires_expiry() {
    let observation = ProviderAccountObservation {
        account_id: "account-1".to_owned(),
        availability: ProviderAccountAvailability::Cooldown,
        availability_reason: None,
        cooldown_until: None,
        provider_quota_json: None,
        availability_observed_at: Utc::now(),
        quota_observed_at: None,
    };
    assert!(observation.validate().is_err());
}

#[test]
fn imported_account_rejects_mismatched_cooldown_runtime_facts() {
    let mut imported = account("acct_invalid_cooldown", "user-invalid-cooldown");
    imported.cooldown_until = Some(Utc::now() + TimeDelta::minutes(5));
    assert!(imported.validate().is_err());
}
