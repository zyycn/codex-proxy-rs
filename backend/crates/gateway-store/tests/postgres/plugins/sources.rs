//! 验证插件更新来源、制品引用、并发安装与代理版本约束

use gateway_admin::{
    model::{
        plugins::{
            PluginSource, PluginSourceEgress,
            distribution::{PluginSourceBinding, PluginUpdatePolicy, PluginUpdateSource},
        },
        proxies::{NewProxy, UpdateProxy},
    },
    ports::{plugins::PluginStore, proxy::ProxyStore, store::AdminStoreErrorKind},
};
use gateway_core::account::OutboundProxy;
use gateway_store::postgres::{PgPluginStore, PgProxyRepository};

use super::{
    super::TestDatabase,
    artifacts::{artifact, context, initialize_revision},
};

#[tokio::test]
async fn upgrades_keep_old_artifacts_and_require_explicit_source_change() {
    let Some(database) = TestDatabase::create("plugin_sources").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let old = artifact('a', &["linux-x86_64"]);
    store
        .install_artifact(old.clone(), PluginSource::Upload, &context())
        .await
        .unwrap();
    let mut new = artifact('b', &["linux-x86_64"]);
    new.metadata.version = "2.0.0".into();
    let remote = PluginSource::Github {
        repository: "example/plugin".into(),
        tag: "v2.0.0".into(),
        asset: "plugin.tar.gz".into(),
        credential_ids: vec![],
        outbound_proxy: None,
    };
    let failure = store
        .install_artifact(new.clone(), remote.clone(), &context())
        .await
        .err()
        .unwrap();
    assert_eq!(failure.kind(), AdminStoreErrorKind::Conflict);
    assert_eq!(store.list_artifacts().await.unwrap().len(), 1);
    store
        .change_update_source(
            PluginSourceBinding {
                plugin_id: "test.example".into(),
                source: PluginUpdateSource::Github {
                    repository: "example/plugin".into(),
                },
                policy: Default::default(),
                outbound_proxy_id: None,
            },
            &context(),
        )
        .await
        .unwrap();
    store
        .install_artifact(new, remote, &context())
        .await
        .unwrap();
    assert_eq!(store.list_artifacts().await.unwrap().len(), 2);
    assert_eq!(
        store
            .load_artifact(&old.metadata.sha256)
            .await
            .unwrap()
            .archive,
        old.archive
    );
    database.close().await;
}

#[tokio::test]
async fn concurrent_identical_installations_are_idempotent_without_duplicate_audit() {
    let Some(database) = TestDatabase::create("plugin_install_race").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let context = context();
    let results = futures::future::join_all((0..8).map(|_| {
        store.install_artifact(
            artifact('d', &["linux-x86_64"]),
            PluginSource::Upload,
            &context,
        )
    }))
    .await;
    assert!(results.iter().all(|result| {
        result
            .as_ref()
            .is_ok_and(|mutation| mutation.config_revision.get() == 2)
    }));
    let count: i64 = sqlx::query_scalar(
        "select count(*) from admin_audit_events where entity_kind='plugin_artifact'",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    database.close().await;
}

#[tokio::test]
async fn last_version_deletion_clears_source_rules_and_allows_a_fresh_install() {
    let Some(database) = TestDatabase::create("plugin_source_cleanup").await else {
        return;
    };
    initialize_revision(&database).await;
    let store = PgPluginStore::new(database.pool.clone());
    let first = artifact('a', &["linux-x86_64"]);
    let mut second = artifact('b', &["linux-x86_64"]);
    second.metadata.version = "2.0.0".into();
    for package in [&first, &second] {
        store
            .install_artifact(package.clone(), PluginSource::Upload, &context())
            .await
            .unwrap();
    }
    store
        .delete_artifact(&first.metadata.sha256, &context())
        .await
        .unwrap();
    assert_eq!(
        store.list_update_sources().await.unwrap()[0].source,
        PluginUpdateSource::Upload
    );
    let remote = PluginSource::Github {
        repository: "example/new-source".into(),
        tag: "v1.0.0".into(),
        asset: "plugin.tar.gz".into(),
        credential_ids: vec![],
        outbound_proxy: None,
    };
    assert_eq!(
        store
            .install_artifact(first.clone(), remote.clone(), &context())
            .await
            .err()
            .unwrap()
            .kind(),
        AdminStoreErrorKind::Conflict
    );

    store
        .delete_artifact(&second.metadata.sha256, &context())
        .await
        .unwrap();
    assert!(store.list_update_sources().await.unwrap().is_empty());
    store
        .install_artifact(first, remote.clone(), &context())
        .await
        .unwrap();
    assert_eq!(
        store.list_update_sources().await.unwrap()[0].source,
        (&remote).into()
    );
    database.close().await;
}

#[tokio::test]
async fn update_policy_is_persistent_audited_and_unchanged_by_install_or_read() {
    let Some(database) = TestDatabase::create("plugin_update_policy").await else {
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
    assert_eq!(
        store.list_update_sources().await.unwrap()[0].policy,
        PluginUpdatePolicy::Manual {}
    );
    let binding = PluginSourceBinding {
        plugin_id: "test.example".into(),
        source: PluginUpdateSource::Github {
            repository: "example/plugin".into(),
        },
        policy: PluginUpdatePolicy::Pinned {
            tag: "v2.0.0-rc.1".into(),
            allow_prerelease: true,
        },
        outbound_proxy_id: None,
    };
    assert_eq!(
        store
            .change_update_source(binding.clone(), &context())
            .await
            .unwrap()
            .get(),
        3
    );
    drop(store);
    let reopened = PgPluginStore::new(database.pool.clone());
    for _ in 0..2 {
        assert_eq!(
            reopened.list_update_sources().await.unwrap(),
            vec![binding.clone()]
        );
    }
    let mut next = artifact('b', &["linux-x86_64"]);
    next.metadata.version = "2.0.0-rc.1".into();
    reopened
        .install_artifact(
            next,
            PluginSource::Github {
                repository: "example/plugin".into(),
                tag: "v2.0.0-rc.1".into(),
                asset: "plugin.tar.gz".into(),
                credential_ids: vec![],
                outbound_proxy: None,
            },
            &context(),
        )
        .await
        .unwrap();
    assert_eq!(reopened.list_update_sources().await.unwrap(), vec![binding]);
    let audits: Vec<(String, i64, Vec<String>)> = sqlx::query_as("select action,config_revision,changed_fields from admin_audit_events where entity_kind='plugin_source'")
        .fetch_all(&database.pool).await.unwrap();
    assert_eq!(
        audits,
        vec![(
            "change_source".into(),
            3,
            vec!["source".into(), "policy".into(), "outbound_proxy".into()]
        )]
    );
    let revision: i64 =
        sqlx::query_scalar("select config_revision from runtime_settings where id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(revision, 4, "读取不能推进配置 revision");
    database.close().await;
}

#[tokio::test]
async fn source_proxy_is_revision_fenced_and_referenced_until_the_artifact_is_deleted() {
    let Some(database) = TestDatabase::create("plugin_source_proxy").await else {
        return;
    };
    initialize_revision(&database).await;
    let plugins = PgPluginStore::new(database.pool.clone());
    let proxies = PgProxyRepository::new(database.pool.clone());
    let saved = proxies
        .create(
            NewProxy {
                auto_location: false,
                test: None,
                location: None,
                name: "Plugin source".into(),
                proxy: OutboundProxy::parse("http://127.0.0.1:18080").unwrap(),
            },
            &context(),
        )
        .await
        .unwrap()
        .record;
    let source = PluginSource::Github {
        repository: "example/plugin".into(),
        tag: "v1.0.0".into(),
        asset: "plugin.tar.gz".into(),
        credential_ids: vec![],
        outbound_proxy: Some(PluginSourceEgress {
            id: saved.id.clone(),
            revision: saved.revision.get(),
        }),
    };
    let installed = plugins
        .install_artifact(artifact('e', &["linux-x86_64"]), source.clone(), &context())
        .await
        .unwrap();
    assert_eq!(
        plugins.list_update_sources().await.unwrap()[0].outbound_proxy_id,
        Some(saved.id.clone())
    );
    let updated = proxies
        .update(
            UpdateProxy {
                auto_location: None,
                test: None,
                location: None,
                id: saved.id.clone(),
                revision: saved.revision,
                name: saved.name,
                proxy: Some(OutboundProxy::parse("http://127.0.0.1:18081").unwrap()),
            },
            &context(),
        )
        .await
        .unwrap()
        .record;
    let mut candidate = artifact('f', &["linux-x86_64"]);
    candidate.metadata.version = "2.0.0".into();
    let Err(error) = plugins
        .install_artifact(candidate, source, &context())
        .await
    else {
        panic!("stale proxy revision must reject the artifact")
    };
    assert_eq!(error.kind(), AdminStoreErrorKind::Conflict);
    assert_eq!(
        proxies
            .delete(&updated.id, updated.revision, &context())
            .await
            .unwrap_err()
            .kind(),
        AdminStoreErrorKind::Conflict
    );
    plugins
        .change_update_source(
            PluginSourceBinding {
                plugin_id: "test.example".into(),
                source: PluginUpdateSource::Upload,
                policy: PluginUpdatePolicy::Manual {},
                outbound_proxy_id: None,
            },
            &context(),
        )
        .await
        .unwrap();
    assert_eq!(
        proxies
            .delete(&updated.id, updated.revision, &context())
            .await
            .unwrap_err()
            .kind(),
        AdminStoreErrorKind::Conflict,
        "历史制品仍引用该代理时不得删除"
    );
    plugins
        .delete_artifact(&installed.artifact.metadata.sha256, &context())
        .await
        .unwrap();
    proxies
        .delete(&updated.id, updated.revision, &context())
        .await
        .unwrap();
    let removed_proxy_source = PluginSource::Github {
        repository: "example/plugin".into(),
        tag: "v3.0.0".into(),
        asset: "plugin.tar.gz".into(),
        credential_ids: vec![],
        outbound_proxy: Some(PluginSourceEgress {
            id: updated.id,
            revision: updated.revision.get(),
        }),
    };
    let mut candidate = artifact('0', &["linux-x86_64"]);
    candidate.metadata.version = "3.0.0".into();
    let Err(error) = plugins
        .install_artifact(candidate, removed_proxy_source, &context())
        .await
    else {
        panic!("deleted frozen proxy identity must reject the artifact")
    };
    assert_eq!(error.kind(), AdminStoreErrorKind::Conflict);
    database.close().await;
}
