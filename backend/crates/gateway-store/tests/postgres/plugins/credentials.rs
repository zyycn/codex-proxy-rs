//! 验证插件下载凭据的保护、引用清理与事务回滚

use gateway_admin::{
    model::plugins::{
        PluginSource,
        distribution::{
            DownloadPurpose, SourceAuthentication, SourceCredential, SourceCredentialInfo,
        },
    },
    ports::{plugins::PluginStore, store::AdminStoreErrorKind},
};
use gateway_store::postgres::PgPluginStore;
use secrecy::ExposeSecret as _;

use super::{
    super::TestDatabase,
    artifacts::{artifact, context, initialize_revision},
};

#[tokio::test]
async fn download_secrets_survive_recovery_without_entering_lists_or_audit_and_references_block_deletion()
 {
    let Some(database) = TestDatabase::create("plugin_credentials").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let id = uuid::Uuid::now_v7().to_string();
    let info = SourceCredentialInfo {
        id: id.clone(),
        name: "Test GitHub".into(),
        origin: "https://api.github.com".into(),
        path_prefix: "/repos/example/plugin".into(),
        purposes: vec![DownloadPurpose::Metadata, DownloadPurpose::Artifact],
    };
    store
        .save_source_credential(
            SourceCredential {
                info: info.clone(),
                authentication: SourceAuthentication::Github {
                    token: "fixture-sensitive-value".into(),
                },
            },
            &context(),
        )
        .await
        .unwrap();
    let listed = store.list_source_credentials().await.unwrap();
    assert_eq!(listed, vec![info]);
    assert!(
        !serde_json::to_string(&listed)
            .unwrap()
            .contains("fixture-sensitive-value")
    );
    drop(store);
    let store = PgPluginStore::new(database.pool.clone());
    let recovered = store.load_source_credential(&id).await.unwrap();
    let SourceAuthentication::Github { token } = recovered.authentication else {
        panic!("wrong authentication type")
    };
    assert_eq!(token.expose_secret(), "fixture-sensitive-value");
    let saved = store
        .install_artifact(
            artifact('c', &["linux-x86_64"]),
            PluginSource::Github {
                repository: "example/plugin".into(),
                tag: "v1.0.0".into(),
                asset: "plugin.tar.gz".into(),
                credential_ids: vec![id.clone()],
                outbound_proxy: None,
            },
            &context(),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .delete_source_credential(&id, &context())
            .await
            .unwrap_err()
            .kind(),
        AdminStoreErrorKind::Conflict
    );
    let audit: Vec<String> =
        sqlx::query_scalar("select row_to_json(events)::text from admin_audit_events events")
            .fetch_all(&database.pool)
            .await
            .unwrap();
    assert!(
        audit
            .iter()
            .all(|event| !event.contains("fixture-sensitive-value"))
    );
    store
        .delete_artifact(&saved.artifact.metadata.sha256, &context())
        .await
        .unwrap();
    assert!(store.list_source_credentials().await.unwrap().is_empty());
    assert!(store.list_update_sources().await.unwrap().is_empty());
    assert!(matches!(
        store.load_source_credential(&id).await,
        Err(error) if error.kind() == AdminStoreErrorKind::NotFound
    ));
    database.close().await;
}

fn credential(id: &str) -> SourceCredential {
    SourceCredential {
        info: SourceCredentialInfo {
            id: id.into(),
            name: "Cleanup fixture".into(),
            origin: "https://downloads.example.com".into(),
            path_prefix: "/".into(),
            purposes: vec![DownloadPurpose::Artifact],
        },
        authentication: SourceAuthentication::Bearer {
            token: "cleanup-fixture-secret".into(),
        },
    }
}

fn source(ids: &[&str]) -> PluginSource {
    PluginSource::Url {
        url: "https://downloads.example.com/plugin.tar.gz".into(),
        credential_ids: ids.iter().map(|id| (*id).into()).collect(),
        outbound_proxy: None,
    }
}

#[tokio::test]
async fn deletion_cleans_exclusive_credentials_but_preserves_shared_and_unrelated_ones() {
    let Some(database) = TestDatabase::create("plugin_credential_cleanup").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let exclusive = uuid::Uuid::now_v7().to_string();
    let shared = uuid::Uuid::now_v7().to_string();
    let unrelated = uuid::Uuid::now_v7().to_string();
    for id in [&exclusive, &shared, &unrelated] {
        store
            .save_source_credential(credential(id), &context())
            .await
            .unwrap();
    }
    let first = artifact('a', &["linux-x86_64"]);
    store
        .install_artifact(first.clone(), source(&[&exclusive, &shared]), &context())
        .await
        .unwrap();
    let mut second = artifact('b', &["linux-x86_64"]);
    second.metadata.plugin_id = "test.other".into();
    second.metadata.name = "other".into();
    store
        .install_artifact(second.clone(), source(&[&shared]), &context())
        .await
        .unwrap();

    let revision = store
        .delete_artifact(&first.metadata.sha256, &context())
        .await
        .unwrap();
    let remaining = store.list_source_credentials().await.unwrap();
    assert_eq!(
        remaining
            .iter()
            .map(|info| info.id.as_str())
            .collect::<Vec<_>>(),
        [&shared, &unrelated]
    );
    assert_eq!(
        store.list_update_sources().await.unwrap()[0].plugin_id,
        "test.other"
    );
    let audits: Vec<(String, i64)> = sqlx::query_as(
        "select entity_kind,config_revision from admin_audit_events where action='delete' order by entity_kind",
    ).fetch_all(&database.pool).await.unwrap();
    let revision = i64::try_from(revision.get()).unwrap();
    assert_eq!(
        audits,
        vec![
            ("plugin_artifact".into(), revision),
            ("plugin_source".into(), revision),
            ("plugin_source_credential".into(), revision),
        ]
    );

    store
        .delete_artifact(&second.metadata.sha256, &context())
        .await
        .unwrap();
    assert_eq!(
        store.list_source_credentials().await.unwrap(),
        vec![credential(&unrelated).info]
    );
    assert!(store.list_update_sources().await.unwrap().is_empty());
    database.close().await;
}

#[tokio::test]
async fn cleanup_failure_rolls_back_artifact_source_credentials_revision_and_audit() {
    let Some(database) = TestDatabase::create("plugin_cleanup_atomic").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let id = uuid::Uuid::now_v7().to_string();
    store
        .save_source_credential(credential(&id), &context())
        .await
        .unwrap();
    let installed = store
        .install_artifact(artifact('c', &["linux-x86_64"]), source(&[&id]), &context())
        .await
        .unwrap();
    let before_sources = store.list_update_sources().await.unwrap();
    sqlx::raw_sql(
        "create function reject_credential_cleanup() returns trigger language plpgsql as $$
         begin raise exception 'cleanup failure fixture'; end $$;
         create trigger reject_credential_cleanup before delete on plugin_source_credentials
         for each row execute function reject_credential_cleanup();",
    )
    .execute(&database.pool)
    .await
    .unwrap();

    assert!(
        store
            .delete_artifact(&installed.artifact.metadata.sha256, &context())
            .await
            .is_err()
    );
    assert_eq!(
        store.list_artifacts().await.unwrap(),
        vec![installed.artifact]
    );
    assert_eq!(store.list_update_sources().await.unwrap(), before_sources);
    assert_eq!(
        store.list_source_credentials().await.unwrap(),
        vec![credential(&id).info]
    );
    let revision: i64 =
        sqlx::query_scalar("select config_revision from runtime_settings where id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(
        u64::try_from(revision).unwrap(),
        installed.config_revision.get()
    );
    let audits: i64 =
        sqlx::query_scalar("select count(*) from admin_audit_events where action='delete'")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(audits, 0);
    database.close().await;
}
