//! 验证插件资源重建与显式 Key 退役的持久幂等边界

use gateway_admin::{
    model::{
        Revision,
        account_groups::{AccountGroupColor, NewAccountGroup},
        client_keys::{DeleteClientKey, NewClientKey},
        plugin_resources::PluginResourceOwner,
        plugins::{PluginSource, instances::PluginInstance},
    },
    ports::{
        plugin_resources::PluginResourceStore as _, plugins::PluginStore as _,
        store::ClientKeyStore as _,
    },
};
use gateway_core::{
    policy::{ClientApiKeyId, PlaintextClientApiKey, RateLimits},
    routing::AccountGroupId,
};
use gateway_store::postgres::{PgAdminClientKeyStore, PgPluginStore};

use super::{
    super::TestDatabase,
    artifacts::{artifact, context, initialize_revision},
};

fn key(id: &str) -> NewClientKey {
    NewClientKey {
        id: ClientApiKeyId::new(id).unwrap(),
        name: "Managed key".into(),
        label: None,
        peer_sharing: Default::default(),
        request_profile_overrides: Default::default(),
        group_ids: Vec::new(),
        limits: RateLimits::unlimited(),
        budget: Default::default(),
        plaintext: format!("synthetic-{id}"),
    }
}

#[tokio::test]
async fn deleted_resources_rebuild_but_retired_resources_remain_disabled_across_ensure_and_restart()
{
    let Some(database) = TestDatabase::create("managed_key_retirement").await else {
        return;
    };
    initialize_revision(&database).await;
    let plugins = PgPluginStore::new(database.pool.clone());
    let installed = plugins
        .install_artifact(
            artifact('d', &["linux-x86_64"]),
            PluginSource::Upload,
            &context(),
        )
        .await
        .unwrap();
    let accepted = plugins
        .accept_artifact(&installed.artifact.metadata.sha256, &context())
        .await
        .unwrap();
    let saved = plugins
        .save_instance(
            PluginInstance {
                id: uuid::Uuid::now_v7().to_string(),
                name: "Resources".into(),
                artifact_sha256: installed.artifact.metadata.sha256,
                enabled: true,
                trusted_process: true,
                configuration: serde_json::json!({}),
                secrets: Default::default(),
                bindings: Vec::new(),
                revision: Revision::new(1).unwrap(),
            },
            accepted.config_revision,
            &context(),
        )
        .await
        .unwrap();
    let owner = PluginResourceOwner {
        instance_id: saved.instance.id,
        artifact_sha256: saved.instance.artifact_sha256,
        revision: saved.instance.revision,
    };
    plugins
        .ensure_group(
            &owner,
            "group".into(),
            NewAccountGroup {
                id: AccountGroupId::new(format!("grp_{}", "3".repeat(32))).unwrap(),
                name: "Managed group".into(),
                description: None,
                color: AccountGroupColor::parse("#2563EBFF").unwrap(),
                fast_mode: Default::default(),
                allow_image_generation: true,
            },
            &context(),
        )
        .await
        .unwrap();
    let created = plugins
        .ensure_key(
            &owner,
            "key".into(),
            vec!["group".into()],
            key("first"),
            &context(),
        )
        .await
        .unwrap();
    assert!(created.value.enabled);
    let keys = PgAdminClientKeyStore::new(database.pool.clone());
    keys.delete_client_key(
        DeleteClientKey {
            id: ClientApiKeyId::new(created.value.id).unwrap(),
        },
        &context(),
    )
    .await
    .unwrap();
    let recreated = plugins
        .ensure_key(
            &owner,
            "key".into(),
            vec!["group".into()],
            key("second"),
            &context(),
        )
        .await
        .unwrap();
    assert_eq!(recreated.value.id, "second");
    assert!(recreated.value.enabled);
    let id = ClientApiKeyId::new("second").unwrap();
    keys.retire_client_key(
        &id,
        PlaintextClientApiKey::new("discarded-synthetic-credential").unwrap(),
        &context(),
    )
    .await
    .unwrap();
    for _ in 0..2 {
        let restarted = PgPluginStore::new(database.pool.clone());
        let current = restarted
            .ensure_key(
                &owner,
                "key".into(),
                vec!["group".into()],
                key("must-not-create"),
                &context(),
            )
            .await
            .unwrap();
        assert_eq!(current.value.id, id.as_str());
        assert!(!current.value.enabled);
        assert!(current.revision.is_none());
    }
    let current_context = context();
    let retired_context = context();
    let (ensured, retired) = tokio::join!(
        plugins.ensure_key(
            &owner,
            "key".into(),
            vec!["group".into()],
            key("concurrent-must-not-create"),
            &current_context
        ),
        keys.retire_client_key(
            &id,
            PlaintextClientApiKey::new("discarded-second-credential").unwrap(),
            &retired_context
        ),
    );
    let ensured = ensured.unwrap();
    retired.unwrap();
    assert_eq!(ensured.value.id, id.as_str());
    assert!(!ensured.value.enabled);
    let count: i64 = sqlx::query_scalar("select count(*) from client_api_keys")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert!(keys.reveal_client_key(&id).await.unwrap().is_none());
    database.close().await;
}
