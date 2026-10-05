//! 验证插件制品身份、信任接受、迁移与安装幂等性

use std::collections::BTreeMap;

use gateway_admin::{
    model::{
        MutationActor, MutationContext,
        plugins::{
            InspectedPluginArtifact, PluginArtifactMetadata, PluginContribution, PluginSource,
            distribution::{PluginSourceBinding, PluginUpdateSource},
        },
    },
    ports::{plugins::PluginStore, store::AdminStoreErrorKind},
};
use gateway_store::postgres::PgPluginStore;

use super::super::TestDatabase;

pub(in crate::postgres) fn artifact(digest: char, platforms: &[&str]) -> InspectedPluginArtifact {
    InspectedPluginArtifact {
        metadata: PluginArtifactMetadata {
            plugin_id: "test.example".into(),
            version: "1.0.0".into(),
            name: "example".into(),
            display_name: "Example".into(),
            publisher: "test".into(),
            author: Some("Tests".into()),
            description: "测试包".into(),
            license: "MIT".into(),
            sha256: digest.to_string().repeat(64),
            platforms: platforms.iter().map(|value| (*value).into()).collect(),
            icon: None,
            contributes: BTreeMap::from([(
                "middleware".into(),
                PluginContribution {
                    id: "test.example.middleware".into(),
                    version: 1,
                    stages: vec!["attempt".into()],
                    input_formats: vec!["openai".into()],
                    output_formats: vec!["openai".into()],
                },
            )]),

            configuration_schema: serde_json::json!({}),
            secret_fields: vec![],
            state_namespaces: vec![],
        },
        archive: b"verified package fixture".as_slice().into(),
    }
}

pub(super) fn context() -> MutationContext {
    MutationContext {
        actor: MutationActor::System,
        request_id: "plugin-test".into(),
    }
}

pub(in crate::postgres) async fn initialize_revision(database: &TestDatabase) {
    sqlx::query(
        "insert into runtime_settings(id, config_revision, updated_at) values (1,1,now()) on conflict do nothing",
    )
    .execute(&database.pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn unknown_metadata_fields_do_not_change_artifact_identity() {
    let Some(database) = TestDatabase::create("plugin_metadata_fields").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let fixture = artifact('a', &["linux-x86_64"]);
    let installed = store
        .install_artifact(fixture.clone(), PluginSource::Upload, &context())
        .await
        .unwrap();
    let stored: serde_json::Value =
        sqlx::query_scalar("select metadata_json from plugin_artifacts where sha256=$1")
            .bind(&fixture.metadata.sha256)
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert!(stored.get("permissionDescriptions").is_none());
    assert!(stored.get("requestedPermissions").is_none());
    let mut extended = stored;
    extended["permissionDescriptions"] =
        serde_json::json!([{"permission": "models", "label": "旧文案"}]);
    extended["requestedPermissions"] = serde_json::json!(["models"]);
    extended["futureMetadata"] = serde_json::json!({"enabled": true});
    extended["contributes"]["middleware"]["futureField"] = serde_json::json!(true);
    sqlx::query("update plugin_artifacts set metadata_json=$2 where sha256=$1")
        .bind(&fixture.metadata.sha256)
        .bind(&extended)
        .execute(&database.pool)
        .await
        .unwrap();
    let repeated = store
        .install_artifact(fixture.clone(), PluginSource::Upload, &context())
        .await
        .unwrap();
    assert_eq!(repeated.config_revision, installed.config_revision);
    assert_eq!(repeated.artifact.metadata, fixture.metadata);
    assert_eq!(
        store.list_artifacts().await.unwrap(),
        vec![installed.artifact]
    );
    let stored: serde_json::Value =
        sqlx::query_scalar("select metadata_json from plugin_artifacts where sha256=$1")
            .bind(&fixture.metadata.sha256)
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(stored, extended, "读取和重复安装不得改写未知字段");
    assert_eq!(
        store
            .load_artifact(&fixture.metadata.sha256)
            .await
            .unwrap()
            .metadata,
        fixture.metadata
    );
    let mut changed = fixture;
    changed.metadata.description.push_str(" changed");
    assert_eq!(
        store
            .install_artifact(changed, PluginSource::Upload, &context())
            .await
            .err()
            .unwrap()
            .kind(),
        AdminStoreErrorKind::Conflict
    );
    database.close().await;
}

#[tokio::test]
async fn trust_migration_removes_only_obsolete_metadata_fields() {
    let Some(database) = TestDatabase::create_through("plugin_trust_upgrade", 19).await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    for (digest, extra) in [
        ('a', serde_json::json!({"requestedPermissions": ["models"]})),
        (
            'b',
            serde_json::json!({"permissionDescriptions": ["旧文案"]}),
        ),
        (
            'c',
            serde_json::json!({"requestedPermissions": [], "permissionDescriptions": []}),
        ),
        ('d', serde_json::json!({})),
    ] {
        // 每个制品使用独立平台，保留同一版本的平台不可变约束
        let fixture = artifact(digest, &[&format!("test-{digest}")]);
        let installed = store
            .install_artifact(fixture.clone(), PluginSource::Upload, &context())
            .await
            .unwrap();
        if digest == 'a' {
            store
                .accept_artifact(&fixture.metadata.sha256, &context())
                .await
                .unwrap();
        }
        sqlx::query("update plugin_artifacts set metadata_json=metadata_json || $2 || '{\"futureMetadata\":true}'::jsonb where sha256=$1")
            .bind(&fixture.metadata.sha256).bind(extra).execute(&database.pool).await.unwrap();
        sqlx::query("insert into plugin_instances(id,artifact_sha256,name,enabled,configuration_json,bindings_json,revision) values ($1,$2,'Example',false,'{}','[]',$3)")
            .bind(uuid::Uuid::now_v7()).bind(&fixture.metadata.sha256)
            .bind(i64::try_from(installed.config_revision.get()).unwrap())
            .execute(&database.pool).await.unwrap();
    }
    let mut expected: Vec<serde_json::Value> =
        sqlx::query_scalar("select to_jsonb(a) from plugin_artifacts a order by sha256")
            .fetch_all(&database.pool)
            .await
            .unwrap();
    for row in &mut expected {
        let metadata = row["metadata_json"].as_object_mut().unwrap();
        metadata.remove("requestedPermissions");
        metadata.remove("permissionDescriptions");
    }
    let instances: Vec<serde_json::Value> =
        sqlx::query_scalar("select to_jsonb(i) from plugin_instances i order by id")
            .fetch_all(&database.pool)
            .await
            .unwrap();
    let revision: i64 =
        sqlx::query_scalar("select config_revision from runtime_settings where id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    for _ in 0..2 {
        super::super::TEST_MIGRATOR
            .run(&database.pool)
            .await
            .unwrap();
        let actual: Vec<serde_json::Value> =
            sqlx::query_scalar("select to_jsonb(a) from plugin_artifacts a order by sha256")
                .fetch_all(&database.pool)
                .await
                .unwrap();
        assert_eq!(
            actual, expected,
            "只清理两个旧字段，保留包体、接受事实与其他元数据"
        );
        let current_instances: Vec<serde_json::Value> =
            sqlx::query_scalar("select to_jsonb(i) from plugin_instances i order by id")
                .fetch_all(&database.pool)
                .await
                .unwrap();
        assert_eq!(current_instances, instances);
        let snapshot = store.load_instances().await.unwrap();
        assert_eq!(snapshot.instances.len(), 4);
        assert_eq!(
            snapshot.config_revision.get(),
            u64::try_from(revision).unwrap()
        );
    }
    database.close().await;
}

#[tokio::test]
async fn artifact_acceptance_is_exact_and_idempotent() {
    let Some(database) = TestDatabase::create("plugin_artifact_acceptance").await else {
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
    assert!(installed.artifact.accepted_at.is_none());

    let accepted = store
        .accept_artifact(&installed.artifact.metadata.sha256, &context())
        .await
        .unwrap();
    let repeated = store
        .accept_artifact(&installed.artifact.metadata.sha256, &context())
        .await
        .unwrap();
    assert!(accepted.artifact.accepted_at.is_some());
    assert_eq!(repeated.artifact, accepted.artifact);
    assert_eq!(repeated.config_revision, accepted.config_revision);
    let accept_audits: i64 = sqlx::query_scalar(
        "select count(*) from admin_audit_events where entity_kind='plugin_artifact' and action='accept'",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(accept_audits, 1);
    database.close().await;
}

#[tokio::test]
async fn plugin_migration_preserves_main_settings_and_accepts_new_installations() {
    let Some(database) = TestDatabase::create_through("plugin_upgrade_from_main", 17).await else {
        return;
    };
    initialize_revision(&database).await;
    // 0022 会把仍为旧默认 3600 的 margin 收敛到 300；预设自定义值，
    // 让本测试继续验证「所有已自定义设置都原样保留」。
    sqlx::query("update runtime_settings set max_concurrent_per_account=0, refresh_margin_seconds=1800 where id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    let before: serde_json::Value =
        sqlx::query_scalar("select to_jsonb(s) from runtime_settings s where id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    super::super::TEST_MIGRATOR
        .run(&database.pool)
        .await
        .unwrap();
    let after: serde_json::Value =
        sqlx::query_scalar("select to_jsonb(s) from runtime_settings s where id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    for (key, value) in before.as_object().unwrap() {
        assert_eq!(after.get(key), Some(value), "existing setting {key}");
    }
    let store = PgPluginStore::new(database.pool.clone());
    store
        .install_artifact(
            artifact('a', &["linux-x86_64"]),
            PluginSource::Upload,
            &context(),
        )
        .await
        .unwrap();
    database.close().await;
}

#[tokio::test]
async fn source_identity_requires_publisher_and_name() {
    let Some(database) = TestDatabase::create("plugin_source_identity").await else {
        return;
    };
    for plugin_id in [
        "legacy-plugin",
        "Publisher.name",
        "publisher.-name",
        "publisher.name.extra",
    ] {
        let error = sqlx::query(
            "insert into plugin_update_sources(plugin_id,source_json) values ($1,'{}')",
        )
        .bind(plugin_id)
        .execute(&database.pool)
        .await
        .expect_err("invalid identity must be rejected by the owning table");
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("23514")
        );
    }
    database.close().await;
}

#[tokio::test]
async fn artifact_bytes_and_audit_survive_repository_recreation() {
    let Some(database) = TestDatabase::create("plugin_recovery").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let expected = artifact('a', &["linux-x86_64"]);
    let saved = store
        .install_artifact(expected.clone(), PluginSource::Upload, &context())
        .await
        .unwrap();
    assert_eq!(saved.config_revision.get(), 2);
    drop(store);
    let reopened = PgPluginStore::new(database.pool.clone());
    let recovered = reopened
        .load_artifact(&expected.metadata.sha256)
        .await
        .unwrap();
    assert_eq!(recovered.archive, expected.archive);
    assert_eq!(recovered.metadata, expected.metadata);
    let audit: (String, i64, Vec<String>) = sqlx::query_as("select action, config_revision, changed_fields from admin_audit_events where entity_kind='plugin_artifact'").fetch_one(&database.pool).await.unwrap();
    assert_eq!(
        audit,
        (
            "install".into(),
            2,
            vec!["artifact".into(), "source".into()]
        )
    );
    database.close().await;
}

#[tokio::test]
async fn overlapping_platform_content_cannot_replace_a_published_version() {
    let Some(database) = TestDatabase::create("plugin_immutable").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    store
        .install_artifact(
            artifact('a', &["linux-x86_64"]),
            PluginSource::Upload,
            &context(),
        )
        .await
        .unwrap();
    let result = store
        .install_artifact(
            artifact('b', &["linux-x86_64", "windows-x86_64"]),
            PluginSource::Upload,
            &context(),
        )
        .await;
    assert!(result.is_err_and(|error| error.kind() == AdminStoreErrorKind::Conflict));
    assert_eq!(store.list_artifacts().await.unwrap().len(), 1);
    let revision: i64 =
        sqlx::query_scalar("select config_revision from runtime_settings where id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(revision, 2, "冲突必须回滚全局 revision");
    database.close().await;
}

#[tokio::test]
async fn unchanged_official_package_survives_host_upgrade_and_rollback() {
    let Some(database) = TestDatabase::create("plugin_builtin_reimport").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let package = artifact('a', &["linux-x86_64"]);
    let saved = store
        .install_artifact(
            package.clone(),
            PluginSource::Builtin {
                release: "3.12.0".into(),
            },
            &context(),
        )
        .await
        .unwrap();
    for release in ["3.13.0", "3.12.0", "3.11.0"] {
        let imported = store
            .install_artifact(
                package.clone(),
                PluginSource::Builtin {
                    release: release.into(),
                },
                &context(),
            )
            .await
            .expect("同一个官方包可随不同宿主发行重复导入");
        assert_eq!(imported.config_revision, saved.config_revision);
        assert_eq!(imported.artifact, saved.artifact, "保留首次安装来源与时间");
    }
    let audit_count: i64 = sqlx::query_scalar(
        "select count(*) from admin_audit_events where entity_kind='plugin_artifact'",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(audit_count, 1, "幂等导入不制造配置变更与重复安装审计");
    database.close().await;
}

#[tokio::test]
async fn official_reimport_still_rejects_changed_metadata_and_custom_source() {
    let Some(database) = TestDatabase::create("plugin_builtin_identity").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let source = PluginSource::Builtin {
        release: "3.12.0".into(),
    };
    let package = artifact('a', &["linux-x86_64"]);
    let saved = store
        .install_artifact(package.clone(), source.clone(), &context())
        .await
        .unwrap();
    let mut changed = package.clone();
    changed.metadata.display_name = "Changed metadata".into();
    for (candidate, source) in [(changed, source), (package, PluginSource::Upload)] {
        assert!(
            store
                .install_artifact(candidate, source, &context())
                .await
                .is_err_and(|error| error.kind() == AdminStoreErrorKind::Conflict)
        );
    }
    assert_eq!(store.list_artifacts().await.unwrap(), vec![saved.artifact]);
    let revision: i64 =
        sqlx::query_scalar("select config_revision from runtime_settings where id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(revision, 2);
    database.close().await;
}

#[tokio::test]
async fn changing_update_binding_does_not_relabel_custom_package_as_official() {
    let Some(database) = TestDatabase::create("plugin_builtin_provenance").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let package = artifact('a', &["linux-x86_64"]);
    let saved = store
        .install_artifact(package.clone(), PluginSource::Upload, &context())
        .await
        .unwrap();
    let changed_revision = store
        .change_update_source(
            PluginSourceBinding {
                plugin_id: package.metadata.plugin_id.clone(),
                source: PluginUpdateSource::Builtin,
                policy: Default::default(),
                outbound_proxy_id: None,
            },
            &context(),
        )
        .await
        .unwrap();
    assert!(
        store
            .install_artifact(
                package,
                PluginSource::Builtin {
                    release: "3.13.0".into(),
                },
                &context(),
            )
            .await
            .is_err_and(|error| error.kind() == AdminStoreErrorKind::Conflict)
    );
    assert_eq!(store.list_artifacts().await.unwrap(), vec![saved.artifact]);
    let revision: i64 =
        sqlx::query_scalar("select config_revision from runtime_settings where id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(u64::try_from(revision).unwrap(), changed_revision.get());
    database.close().await;
}

#[tokio::test]
async fn authorized_source_change_reuses_identical_package_without_rewriting_provenance() {
    let Some(database) = TestDatabase::create("plugin_reinstall_source").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let package = artifact('a', &["linux-x86_64"]);
    let saved = store
        .install_artifact(package.clone(), PluginSource::Upload, &context())
        .await
        .unwrap();
    let source = PluginSource::Url {
        url: "https://example.com/plugin.tar.gz".into(),
        credential_ids: vec![],
        outbound_proxy: None,
    };
    assert!(
        store
            .install_artifact(package.clone(), source.clone(), &context())
            .await
            .is_err_and(|error| error.kind() == AdminStoreErrorKind::Conflict),
        "未经管理员确认的来源仍需拒绝"
    );
    let revision = store
        .change_update_source(
            PluginSourceBinding {
                plugin_id: package.metadata.plugin_id.clone(),
                source: PluginUpdateSource::Url {
                    url: "https://example.com/plugin.tar.gz".into(),
                },
                policy: Default::default(),
                outbound_proxy_id: None,
            },
            &context(),
        )
        .await
        .unwrap();
    let repeated = store
        .install_artifact(package, source, &context())
        .await
        .unwrap();
    assert_eq!(repeated.artifact, saved.artifact, "保留首次安装来源和时间");
    assert_eq!(
        repeated.config_revision, revision,
        "相同包不产生额外版本变更"
    );
    assert_eq!(store.list_artifacts().await.unwrap().len(), 1);
    database.close().await;
}
