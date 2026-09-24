use std::collections::BTreeMap;

use gateway_admin::{
    model::{
        Revision,
        client_keys::DeleteClientKey,
        plugins::{
            PluginSource,
            instances::{
                PluginCapabilityBinding, PluginFailurePolicy, PluginFrontendIdentityBinding,
                PluginInstance, PluginInstanceReplacement,
            },
            state::{PluginStateCommit, PluginStateConfiguration},
        },
    },
    ports::{
        plugins::PluginStore,
        store::{AdminStoreErrorKind, ClientKeyStore},
    },
};
use gateway_core::policy::ClientApiKeyId;
use gateway_store::postgres::{PgAdminClientKeyStore, PgPluginStore};
use secrecy::ExposeSecret as _;

use super::{
    super::TestDatabase,
    artifacts::{artifact, context, initialize_revision},
};

fn instance(digest: String) -> PluginInstance {
    PluginInstance {
        id: uuid::Uuid::now_v7().to_string(),
        name: "Example".into(),
        artifact_sha256: digest,
        enabled: false,
        trusted_process: true,
        configuration: serde_json::json!({"baseUrl":"https://example.com"}),
        secrets: BTreeMap::from([("token".into(), "sensitive-fixture".into())]),
        grants: vec![],
        bindings: vec![],
        revision: Revision::new(1).unwrap(),
    }
}

#[tokio::test]
async fn management_authorization_checks_exact_target_without_loading_secrets() {
    use gateway_admin::model::plugins::management::PluginManagementTarget;
    let Some(database) = TestDatabase::create("plugin_target_authorization").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let installed = store
        .install_artifact(
            artifact('a', &["linux-x86_64"]),
            PluginSource::Upload,
            &context(),
        )
        .await
        .unwrap();
    let accepted = store
        .accept_artifact(&installed.artifact.metadata.sha256, &context())
        .await
        .unwrap();
    let mut candidate = instance(installed.artifact.metadata.sha256);
    candidate.enabled = true;
    let saved = store
        .save_instance(candidate, accepted.config_revision, &context())
        .await
        .unwrap();
    let target = PluginManagementTarget {
        instance_id: saved.instance.id.clone(),
        artifact_sha256: saved.instance.artifact_sha256.clone(),
        revision: saved.instance.revision.get(),
    };
    // 故意留下不能解码为密钥映射的测试数据，授权查询不得读取或解释它。
    sqlx::query("update plugin_instance_secrets set secrets_json=$2 where instance_id=$1")
        .bind(uuid::Uuid::parse_str(&target.instance_id).unwrap())
        .bind(serde_json::json!({"token": false}))
        .execute(&database.pool)
        .await
        .unwrap();
    assert!(store.management_target_is_current(&target).await.unwrap());
    for changed in [
        PluginManagementTarget {
            revision: target.revision + 1,
            ..target.clone()
        },
        PluginManagementTarget {
            revision: u64::MAX,
            ..target.clone()
        },
        PluginManagementTarget {
            artifact_sha256: "b".repeat(64),
            ..target.clone()
        },
        PluginManagementTarget {
            instance_id: uuid::Uuid::now_v7().to_string(),
            ..target.clone()
        },
        PluginManagementTarget {
            instance_id: "invalid".into(),
            ..target.clone()
        },
    ] {
        assert!(!store.management_target_is_current(&changed).await.unwrap());
    }
    sqlx::query("update plugin_instances set enabled=false where id=$1")
        .bind(uuid::Uuid::parse_str(&target.instance_id).unwrap())
        .execute(&database.pool)
        .await
        .unwrap();
    assert!(!store.management_target_is_current(&target).await.unwrap());
    sqlx::query("update plugin_instances set enabled=true where id=$1")
        .bind(uuid::Uuid::parse_str(&target.instance_id).unwrap())
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("update plugin_artifacts set accepted_at=null where sha256=$1")
        .bind(&target.artifact_sha256)
        .execute(&database.pool)
        .await
        .unwrap();
    assert!(!store.management_target_is_current(&target).await.unwrap());
    store
        .delete_instance(&target.instance_id, saved.config_revision, &context())
        .await
        .expect_err("启用中的实例不能删除");
    sqlx::query("update plugin_instances set enabled=false where id=$1")
        .bind(uuid::Uuid::parse_str(&target.instance_id).unwrap())
        .execute(&database.pool)
        .await
        .unwrap();
    store
        .delete_instance(&target.instance_id, saved.config_revision, &context())
        .await
        .unwrap();
    assert!(!store.management_target_is_current(&target).await.unwrap());
    database.close().await;
}

fn usage_binding(client_key_id: &str, account_group_id: &str) -> PluginCapabilityBinding {
    PluginCapabilityBinding {
        contribution: "test.example.usage".into(),
        stage: "observation".into(),
        order: 0,
        failure_policy: PluginFailurePolicy::Observe,
        client_key_ids: vec![client_key_id.into()],
        account_group_ids: vec![account_group_id.into()],
        provider_ids: vec![],
        models: vec![],
        identity_bindings: vec![],
    }
}

fn frontend_authentication_binding(
    principal: &str,
    client_key_id: &str,
) -> PluginCapabilityBinding {
    PluginCapabilityBinding {
        contribution: "test.example.frontendAuthentication".into(),
        stage: "authentication".into(),
        order: 0,
        failure_policy: PluginFailurePolicy::Reject,
        client_key_ids: vec![],
        account_group_ids: vec![],
        provider_ids: vec![],
        models: vec![],
        identity_bindings: vec![PluginFrontendIdentityBinding {
            principal: principal.into(),
            client_key_id: client_key_id.into(),
        }],
    }
}

#[tokio::test]
async fn replacement_commits_enablement_together_and_preserves_old_configuration_and_secrets() {
    let Some(database) = TestDatabase::create("plugin_replace").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let installed = store
        .install_artifact(
            artifact('e', &["linux-x86_64"]),
            PluginSource::Upload,
            &context(),
        )
        .await
        .unwrap();
    let accepted = store
        .accept_artifact(&installed.artifact.metadata.sha256, &context())
        .await
        .unwrap();
    let mut old = instance(installed.artifact.metadata.sha256.clone());
    old.enabled = true;
    let saved = store
        .save_instance(old.clone(), accepted.config_revision, &context())
        .await
        .unwrap();
    let mut current = instance(installed.artifact.metadata.sha256);
    current.enabled = true;
    let switched = store
        .save_instance_replacing(
            current.clone(),
            saved.config_revision,
            PluginStateCommit {
                configuration: PluginStateConfiguration { namespaces: vec![] },
                transition_id: None,
            },
            &[PluginInstanceReplacement {
                id: old.id.clone(),
                expected_revision: saved.instance.revision.get(),
            }],
            &context(),
        )
        .await
        .unwrap();
    let snapshot = store.load_instances().await.unwrap();
    assert_eq!(snapshot.config_revision, switched.config_revision);
    let previous = snapshot
        .instances
        .iter()
        .find(|item| item.id == old.id)
        .unwrap();
    let target = snapshot
        .instances
        .iter()
        .find(|item| item.id == current.id)
        .unwrap();
    assert!(!previous.enabled);
    assert!(target.enabled);
    assert_eq!(previous.revision, target.revision);
    assert_eq!(previous.configuration, old.configuration);
    assert_eq!(
        previous.secrets["token"].expose_secret(),
        old.secrets["token"].expose_secret()
    );
    let audits: i64 =
        sqlx::query_scalar("select count(*) from admin_audit_events where config_revision=$1")
            .bind(i64::try_from(switched.config_revision.get()).unwrap())
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(audits, 2);
    database.close().await;
}

#[tokio::test]
async fn replacement_rolls_back_all_writes_when_a_confirmed_configuration_has_changed() {
    let Some(database) = TestDatabase::create("plugin_replace_stale").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let installed = store
        .install_artifact(
            artifact('f', &["linux-x86_64"]),
            PluginSource::Upload,
            &context(),
        )
        .await
        .unwrap();
    let accepted = store
        .accept_artifact(&installed.artifact.metadata.sha256, &context())
        .await
        .unwrap();
    let mut first = instance(installed.artifact.metadata.sha256.clone());
    first.enabled = true;
    let first = store
        .save_instance(first, accepted.config_revision, &context())
        .await
        .unwrap();
    let mut second = instance(installed.artifact.metadata.sha256.clone());
    second.enabled = true;
    let second = store
        .save_instance(second, first.config_revision, &context())
        .await
        .unwrap();
    let mut target = instance(installed.artifact.metadata.sha256);
    target.enabled = true;
    let result = store
        .save_instance_replacing(
            target,
            second.config_revision,
            PluginStateCommit {
                configuration: PluginStateConfiguration { namespaces: vec![] },
                transition_id: None,
            },
            &[
                PluginInstanceReplacement {
                    id: first.instance.id.clone(),
                    expected_revision: first.instance.revision.get(),
                },
                PluginInstanceReplacement {
                    id: second.instance.id.clone(),
                    expected_revision: second.instance.revision.get() - 1,
                },
            ],
            &context(),
        )
        .await;
    assert_eq!(result.err().unwrap().kind(), AdminStoreErrorKind::Conflict);
    let snapshot = store.load_instances().await.unwrap();
    assert_eq!(snapshot.config_revision, second.config_revision);
    assert_eq!(snapshot.instances.len(), 2);
    assert!(snapshot.instances.iter().all(|item| item.enabled));
    database.close().await;
}

#[tokio::test]
async fn instance_save_rejects_unaccepted_enablement_and_forged_acceptance_facts() {
    let Some(database) = TestDatabase::create("plugin_instance_acceptance_facts").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let mut package = artifact('d', &["linux-x86_64"]);
    package.metadata.requested_permissions = vec!["network".into()];
    let installed = store
        .install_artifact(package, PluginSource::Upload, &context())
        .await
        .unwrap();

    let mut enabled = instance(installed.artifact.metadata.sha256.clone());
    enabled.enabled = true;
    enabled.trusted_process = false;
    assert_eq!(
        store
            .save_instance(enabled, installed.config_revision, &context())
            .await
            .err()
            .expect("unaccepted artifact cannot be enabled")
            .kind(),
        AdminStoreErrorKind::Invalid
    );
    let mut forged = instance(installed.artifact.metadata.sha256.clone());
    forged.trusted_process = true;
    assert_eq!(
        store
            .save_instance(forged, installed.config_revision, &context())
            .await
            .err()
            .expect("unaccepted artifact cannot forge trust")
            .kind(),
        AdminStoreErrorKind::Invalid
    );

    let accepted = store
        .accept_artifact(&installed.artifact.metadata.sha256, &context())
        .await
        .unwrap();
    let missing_grant = instance(installed.artifact.metadata.sha256);
    assert_eq!(
        store
            .save_instance(missing_grant, accepted.config_revision, &context())
            .await
            .err()
            .expect("accepted artifact grants cannot be forged")
            .kind(),
        AdminStoreErrorKind::Invalid
    );
    database.close().await;
}

#[tokio::test]
async fn instance_and_secret_restore_together_and_artifact_deletion_respects_references() {
    let Some(database) = TestDatabase::create("plugin_instances").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let artifact = store
        .install_artifact(
            artifact('e', &["linux-x86_64"]),
            PluginSource::Upload,
            &context(),
        )
        .await
        .unwrap();
    let artifact = store
        .accept_artifact(&artifact.artifact.metadata.sha256, &context())
        .await
        .unwrap();
    let input = instance(artifact.artifact.metadata.sha256.clone());
    let saved = store
        .save_instance(input.clone(), artifact.config_revision, &context())
        .await
        .unwrap();
    drop(store);
    let store = PgPluginStore::new(database.pool.clone());
    let loaded = store.load_instances().await.unwrap();
    assert_eq!(loaded.config_revision, saved.config_revision);
    assert_eq!(loaded.instances[0].configuration, input.configuration);
    assert_eq!(
        loaded.instances[0].secrets["token"].expose_secret(),
        "sensitive-fixture"
    );
    let configuration: String =
        sqlx::query_scalar("select configuration_json::text from plugin_instances")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert!(!configuration.contains("sensitive-fixture"));
    assert_eq!(
        store
            .delete_artifact(&input.artifact_sha256, &context())
            .await
            .unwrap_err()
            .kind(),
        AdminStoreErrorKind::Conflict
    );
    store
        .delete_instance(&input.id, loaded.config_revision, &context())
        .await
        .unwrap();
    store
        .delete_artifact(&input.artifact_sha256, &context())
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("select count(*) from plugin_instance_secrets")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    database.close().await;
}

#[tokio::test]
async fn a_prepared_instance_cannot_commit_over_a_later_configuration_change() {
    let Some(database) = TestDatabase::create("plugin_instance_race").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let artifact = store
        .install_artifact(
            artifact('f', &["linux-x86_64"]),
            PluginSource::Upload,
            &context(),
        )
        .await
        .unwrap();
    let artifact = store
        .accept_artifact(&artifact.artifact.metadata.sha256, &context())
        .await
        .unwrap();
    let first = instance(artifact.artifact.metadata.sha256);
    let mut stale = first.clone();
    stale.name = "stale".into();
    let saved = store
        .save_instance(first.clone(), artifact.config_revision, &context())
        .await
        .unwrap();
    let error = store
        .save_instance(stale, artifact.config_revision, &context())
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), AdminStoreErrorKind::Conflict);
    let loaded = store.load_instances().await.unwrap();
    assert_eq!(loaded.instances[0].name, first.name);
    assert_eq!(loaded.config_revision, saved.config_revision);
    database.close().await;
}

#[tokio::test]
async fn missing_binding_references_reject_without_bumping_revision() {
    let Some(database) = TestDatabase::create("plugin_instance_missing_scope").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let artifact = store
        .install_artifact(
            artifact('a', &["linux-x86_64"]),
            PluginSource::Upload,
            &context(),
        )
        .await
        .unwrap();
    let artifact = store
        .accept_artifact(&artifact.artifact.metadata.sha256, &context())
        .await
        .unwrap();
    let mut input = instance(artifact.artifact.metadata.sha256);
    input.enabled = true;
    input.bindings = vec![usage_binding(
        "missing-client-key",
        "grp_11111111111111111111111111111111",
    )];
    let error = match store
        .save_instance(input, artifact.config_revision, &context())
        .await
    {
        Ok(_) => panic!("missing binding references must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), AdminStoreErrorKind::Invalid);
    let revision: i64 =
        sqlx::query_scalar("select config_revision from runtime_settings where id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(
        revision,
        i64::try_from(artifact.config_revision.get()).unwrap()
    );
    let count: i64 = sqlx::query_scalar("select count(*) from plugin_instances")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    database.close().await;
}

#[tokio::test]
async fn existing_binding_references_roundtrip_in_the_instance_snapshot() {
    let Some(database) = TestDatabase::create("plugin_instance_existing_scope").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let artifact = store
        .install_artifact(
            artifact('b', &["linux-x86_64"]),
            PluginSource::Upload,
            &context(),
        )
        .await
        .unwrap();
    let artifact = store
        .accept_artifact(&artifact.artifact.metadata.sha256, &context())
        .await
        .unwrap();
    let client_key_id = "client-key-observer";
    let account_group_id = "grp_22222222222222222222222222222222";
    let plaintext_key = format!("sk_{}", "a".repeat(43));
    sqlx::query(
        "insert into client_api_keys(
           id, name, key, enabled, max_concurrency, requests_per_minute, created_at, updated_at
         ) values ($1, 'Observer Key', $2, true, 0, 0, now(), now())",
    )
    .bind(client_key_id)
    .bind(plaintext_key)
    .execute(&database.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into account_groups(
           id, name, description, enabled, created_at, updated_at, color
         ) values ($1, 'Observer Group', null, false, now(), now(), '#112233FF')",
    )
    .bind(account_group_id)
    .execute(&database.pool)
    .await
    .unwrap();

    let mut input = instance(artifact.artifact.metadata.sha256);
    input.enabled = true;
    input.bindings = vec![usage_binding(client_key_id, account_group_id)];
    let saved = store
        .save_instance(input, artifact.config_revision, &context())
        .await
        .unwrap();
    let loaded = store.load_instances().await.unwrap();
    assert_eq!(loaded.instances.len(), 1);
    assert_eq!(
        loaded.instances[0].bindings[0].client_key_ids,
        [client_key_id]
    );
    assert_eq!(
        loaded.instances[0].bindings[0].account_group_ids,
        [account_group_id]
    );

    sqlx::query("delete from client_api_keys where id = $1")
        .bind(client_key_id)
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("delete from account_groups where id = $1")
        .bind(account_group_id)
        .execute(&database.pool)
        .await
        .unwrap();
    let mut disabled = loaded.instances[0].clone();
    disabled.enabled = false;
    store
        .save_instance(disabled, saved.config_revision, &context())
        .await
        .expect("disabling must preserve stale bindings as a recovery path");
    let disabled = store.load_instances().await.unwrap().instances.remove(0);
    assert!(!disabled.enabled);
    assert_eq!(disabled.bindings[0].client_key_ids, [client_key_id]);
    assert_eq!(disabled.bindings[0].account_group_ids, [account_group_id]);
    database.close().await;
}

#[tokio::test]
async fn client_key_deletion_revision_prevents_a_stale_authentication_mapping_commit() {
    let Some(database) = TestDatabase::create("plugin_instance_auth_key_race").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let artifact = store
        .install_artifact(
            artifact('c', &["linux-x86_64"]),
            PluginSource::Upload,
            &context(),
        )
        .await
        .unwrap();
    let artifact = store
        .accept_artifact(&artifact.artifact.metadata.sha256, &context())
        .await
        .unwrap();
    let client_key_id = "client-key-authentication";
    sqlx::query(
        "insert into client_api_keys(
           id, name, key, enabled, max_concurrency, requests_per_minute, created_at, updated_at
         ) values ($1, 'Authentication Key', $2, true, 0, 0, now(), now())",
    )
    .bind(client_key_id)
    .bind(format!("sk_{}", "b".repeat(43)))
    .execute(&database.pool)
    .await
    .unwrap();

    let mut input = instance(artifact.artifact.metadata.sha256);
    input.enabled = true;
    input.bindings = vec![frontend_authentication_binding(
        "controlled-principal",
        client_key_id,
    )];
    let saved = store
        .save_instance(input.clone(), artifact.config_revision, &context())
        .await
        .expect("existing mapped key is accepted");
    let loaded = store.load_instances().await.unwrap();
    assert_eq!(
        loaded.instances[0].bindings[0].identity_bindings[0].client_key_id,
        client_key_id
    );

    let keys = PgAdminClientKeyStore::new(database.pool.clone());
    let deleted_revision = keys
        .delete_client_key(
            DeleteClientKey {
                id: ClientApiKeyId::new(client_key_id).unwrap(),
            },
            &context(),
        )
        .await
        .expect("deletion is an explicit revocation");
    assert!(deleted_revision.get() > saved.config_revision.get());

    let stale_error = match store
        .save_instance(input.clone(), saved.config_revision, &context())
        .await
    {
        Ok(_) => panic!("stale candidate cannot commit over key deletion"),
        Err(error) => error,
    };
    assert_eq!(stale_error.kind(), AdminStoreErrorKind::Conflict);
    let missing_error = match store
        .save_instance(input.clone(), deleted_revision, &context())
        .await
    {
        Ok(_) => panic!("an enabled binding cannot reference the deleted key"),
        Err(error) => error,
    };
    assert_eq!(missing_error.kind(), AdminStoreErrorKind::Invalid);

    input.enabled = false;
    store
        .save_instance(input, deleted_revision, &context())
        .await
        .expect("disabling preserves the stale mapping as a recovery path");
    database.close().await;
}

#[tokio::test]
async fn version_settings_survive_upgrade_and_disabled_edits_and_share_the_transaction() {
    let Some(database) = TestDatabase::create("plugin_version_settings").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let mut revision = Revision::new(1).unwrap();
    for digest in ['a', 'b'] {
        let mut package = artifact(digest, &["linux-x86_64"]);
        package.metadata.version = if digest == 'a' { "1.0.0" } else { "2.0.0" }.into();
        store
            .install_artifact(package, PluginSource::Upload, &context())
            .await
            .unwrap();
        revision = store
            .accept_artifact(&digest.to_string().repeat(64), &context())
            .await
            .unwrap()
            .config_revision;
    }
    let mut old = instance("a".repeat(64));
    old.enabled = true;
    old.configuration = serde_json::json!({"v1":"original"});
    let saved = store
        .save_instance(old, revision, &context())
        .await
        .unwrap();
    let id = saved.instance.id.clone();
    let mut next = saved.instance;
    next.artifact_sha256 = "b".repeat(64);
    next.configuration = serde_json::json!({"v2":"changed"});
    next.secrets.insert("token".into(), "v2-secret".into());
    let saved = store
        .save_instance(next, saved.config_revision, &context())
        .await
        .unwrap();
    let old_settings = store
        .load_version_configuration(&id, &"a".repeat(64))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        old_settings.configuration,
        serde_json::json!({"v1":"original"})
    );
    assert_eq!(
        old_settings.secrets["token"].expose_secret(),
        "sensitive-fixture"
    );
    let mut disabled = saved.instance;
    disabled.enabled = false;
    disabled.configuration = serde_json::json!({"incomplete":"draft"});
    disabled.secrets.clear();
    let saved = store
        .save_instance(disabled, saved.config_revision, &context())
        .await
        .unwrap();
    let v2_settings = store
        .load_version_configuration(&id, &"b".repeat(64))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        v2_settings.configuration,
        serde_json::json!({"v2":"changed"})
    );
    assert_eq!(v2_settings.secrets["token"].expose_secret(), "v2-secret");
    let mut conflict = saved.instance;
    conflict.enabled = true;
    conflict.configuration = serde_json::json!({"mustNotCommit":true});
    assert!(
        store
            .save_instance(conflict, revision, &context())
            .await
            .is_err()
    );
    assert_eq!(
        store
            .load_version_configuration(&id, &"b".repeat(64))
            .await
            .unwrap()
            .unwrap()
            .configuration,
        v2_settings.configuration
    );
    assert_eq!(store.configuration_versions(&id).await.unwrap().len(), 2);
    store
        .delete_artifact(&"a".repeat(64), &context())
        .await
        .unwrap();
    assert!(
        store
            .load_version_configuration(&id, &"a".repeat(64))
            .await
            .unwrap()
            .is_none()
    );
    let current = store.load_instances().await.unwrap();
    store
        .delete_instance(&id, current.config_revision, &context())
        .await
        .unwrap();
    assert!(store.configuration_versions(&id).await.unwrap().is_empty());
    database.close().await;
}

#[tokio::test]
async fn version_configuration_migration_preserves_existing_enabled_settings() {
    let Some(database) = TestDatabase::create_through("plugin_version_upgrade", 18).await else {
        return;
    };
    initialize_revision(&database).await;
    let id = uuid::Uuid::now_v7();
    let digest = "a".repeat(64);
    let store = PgPluginStore::new(database.pool.clone());
    let package = store
        .install_artifact(
            artifact('a', &["linux-x86_64"]),
            PluginSource::Upload,
            &context(),
        )
        .await
        .unwrap();
    store
        .accept_artifact(&package.artifact.metadata.sha256, &context())
        .await
        .unwrap();
    sqlx::query("insert into plugin_instances (id,artifact_sha256,name,enabled,configuration_json,bindings_json,revision) values ($1,$2,'prior installation',true,$3,'[]'::jsonb,1)")
        .bind(id).bind(&digest).bind(serde_json::json!({"preserved":true})).execute(&database.pool).await.unwrap();
    sqlx::query("insert into plugin_instance_secrets (instance_id,secrets_json) values ($1,$2)")
        .bind(id)
        .bind(serde_json::json!({"token":"migration-fixture-secret"}))
        .execute(&database.pool)
        .await
        .unwrap();
    super::super::TEST_MIGRATOR
        .run(&database.pool)
        .await
        .unwrap();
    let saved = store
        .load_version_configuration(&id.to_string(), &digest)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.configuration, serde_json::json!({"preserved":true}));
    assert_eq!(
        saved.secrets["token"].expose_secret(),
        "migration-fixture-secret"
    );
    database.close().await;
}
