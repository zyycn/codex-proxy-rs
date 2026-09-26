use crate::support::environment::{Environment, account_grant, mutation};
use gateway_admin::{
    model::{
        account_groups::{AccountGroupColor, CreateAccountGroup},
        plugin_resources::{GroupMembersChange, ManagedKeyConfig, PluginResourceOwner},
    },
    ports::plugin_resources::PluginResourceAccess,
};
use gateway_core::{
    lifecycle::CancellationToken,
    task::{WorkerContribution, WorkerRunnable},
};
use gateway_plugin_runtime::PluginRuntime;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

fn group(name: &str) -> CreateAccountGroup {
    CreateAccountGroup {
        name: name.into(),
        description: None,
        disable_fast: false,
        color: AccountGroupColor::parse("#2563EBFF").unwrap(),
    }
}

fn key() -> ManagedKeyConfig {
    ManagedKeyConfig {
        name: " managed key ".into(),
        limits: gateway_core::policy::RateLimits::unlimited(),
        budget: Default::default(),
    }
}

async fn owner(environment: &Environment) -> PluginResourceOwner {
    let instance = environment
        .store
        .admin_ports()
        .plugins()
        .load_instances()
        .await
        .unwrap()
        .instances
        .remove(0);
    PluginResourceOwner {
        instance_id: instance.id,
        artifact_sha256: instance.artifact_sha256,
        revision: instance.revision,
    }
}

async fn import_account(
    environment: &Environment,
    core: &gateway_core::CoreBundle,
) -> gateway_core::account::ProviderAccountId {
    use gateway_admin::model::provider_credentials::{
        PreparedCredentialCreate, PreparedPluginAccountSave, ProviderDocument,
    };
    let access = gateway_admin::initialize_plugin_accounts(
        crate::support::native::admin_registry(),
        environment.store.admin_ports().accounts(),
        core.snapshot_control(),
    );
    let result = access
        .save(
            PreparedPluginAccountSave::Create(PreparedCredentialCreate {
                account_id: gateway_core::account::ProviderAccountId::new(format!(
                    "acct_{}",
                    uuid::Uuid::new_v4().simple()
                ))
                .unwrap(),
                provider_kind: gateway_core::routing::ProviderKind::new("openai").unwrap(),
                name: "imported maintenance fixture".into(),
                email: None,
                upstream_user_id: None,
                upstream_account_id: None,
                plan_type: None,
                authentication_kind: "api_key".into(),
                provider_material: ProviderDocument::new(
                    gateway_core::account::OpaqueProviderData::new(
                        json!({"key":"test-only"}).as_object().unwrap().clone(),
                    ),
                ),
                model_access: None,
                outbound_proxy: None,
                has_refresh_token: false,
                access_token_expires_at: None,
                next_refresh_at: None,
                enabled: true,
                credential_state: gateway_core::account::CredentialState::Ready,
                credential_observed_at: chrono::Utc::now(),
            }),
            &mutation(),
        )
        .await
        .unwrap();
    result.account_id
}

async fn revision(environment: &Environment) -> gateway_admin::model::Revision {
    environment
        .store
        .admin_ports()
        .plugins()
        .load_instances()
        .await
        .unwrap()
        .config_revision
}

async fn start(
    environment: &Environment,
) -> (
    Arc<PluginRuntime>,
    gateway_core::CoreBundle,
    Arc<dyn PluginResourceAccess>,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let (runtime, core) = environment.runtime().await;
    let resources = gateway_admin::initialize_plugin_resources(
        environment.store.admin_ports().plugin_resources(),
        core.snapshot_control(),
    );
    runtime.bind_resource_ports(&resources).unwrap();
    let WorkerContribution::Registration(registration) =
        runtime.maintenance_worker(core.snapshots()).unwrap()
    else {
        panic!("maintenance registration")
    };
    let WorkerRunnable::Daemon { task, .. } = registration.runnable else {
        panic!("maintenance daemon")
    };
    let stop = CancellationToken::new();
    let cancellation = stop.clone();
    let task = tokio::spawn(async move {
        task.run(cancellation).await.unwrap();
    });
    (runtime, core, resources, stop, task)
}

async fn wait_done(path: &std::path::Path, after: usize) -> Value {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let entries: Vec<Value> = std::fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();
            let done: Vec<_> = entries
                .iter()
                .filter(|value| value["phase"] == "done")
                .collect();
            if done.len() > after {
                return (*done.last().unwrap()).clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("plugin reconciliation finished")
}

#[tokio::test]
async fn published_reconciliation_provisions_retries_imports_and_restores_without_overwriting_groups()
 {
    let Some(environment) = Environment::create().await else {
        return;
    };
    let account = environment.account(None).await;
    let original = environment.account_group_with_account(&account).await;
    let marker = environment.directory.path().join("maintenance.jsonl");
    let failed = environment.directory.path().join("failed-once");
    environment.install_plugin(json!({"maintenance_fixture":true, "maintenance_marker":marker, "maintenance_fail_once":failed}), vec![account_grant("groups"), account_grant("keys"), account_grant("data")]).await;
    let (runtime, core, resources, stop, task) = start(&environment).await;
    let first = wait_done(&marker, 0).await;
    assert!(failed.exists());
    let group_id =
        gateway_core::routing::AccountGroupId::new(first["group"].as_str().unwrap()).unwrap();
    let members = environment
        .store
        .admin_ports()
        .account_groups()
        .load_account_group_members(&[original.clone(), group_id.clone()])
        .await
        .unwrap();
    assert_eq!(
        members
            .iter()
            .filter(|m| m.account_id == account.as_str())
            .count(),
        2
    );
    let owner = owner(&environment).await;
    let before = revision(&environment).await;
    let same = resources
        .ensure_group(
            &owner,
            "pool".into(),
            group("ignored replacement name"),
            &mutation(),
        )
        .await
        .unwrap();
    assert_eq!(same.id, first["group"]);
    assert_eq!(same.name, "plugin fixture group");
    assert_eq!(before, revision(&environment).await);
    let imported = import_account(&environment, &core).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let members = environment
                .store
                .admin_ports()
                .account_groups()
                .load_account_group_members(std::slice::from_ref(&group_id))
                .await
                .unwrap();
            if members
                .iter()
                .any(|member| member.account_id == imported.as_str())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    stop.cancel();
    task.await.unwrap();
    runtime.shutdown().await;
    environment.release_plugin_accounts(&runtime);
    drop(resources);
    drop(core);
    drop(runtime);
    // 离线新增账号后重新启动真实插件子进程；稳定资源键必须复用原分组与 Key。
    let offline = environment.account(None).await;
    std::fs::write(&marker, "").unwrap();
    let (runtime, core, resources, stop, task) = start(&environment).await;
    let restored = wait_done(&marker, 0).await;
    assert_eq!(restored["group"], first["group"]);
    assert_eq!(restored["key"], first["key"]);
    let members = environment
        .store
        .admin_ports()
        .account_groups()
        .load_account_group_members(std::slice::from_ref(&group_id))
        .await
        .unwrap();
    assert!(members.iter().any(|m| m.account_id == offline.as_str()));
    let store = environment.store.admin_ports().plugins();
    let snapshot = store.load_instances().await.unwrap();
    let mut instance = snapshot.instances[0].clone();
    instance.enabled = false;
    let disabled = store
        .save_instance(instance, snapshot.config_revision, &mutation())
        .await
        .unwrap();
    core.snapshot_control()
        .publish_committed(
            gateway_core::routing::ConfigRevision::new(disabled.config_revision.get()).unwrap(),
        )
        .await;
    assert!(
        resources
            .change_members(
                &owner,
                GroupMembersChange {
                    resource_key: "pool".into(),
                    add: vec![],
                    remove: vec![account.as_str().to_owned()]
                },
                &mutation()
            )
            .await
            .is_err()
    );
    stop.cancel();
    task.await.unwrap();
    runtime.shutdown().await;
    environment.release_plugin_accounts(&runtime);
    drop(resources);
    drop(core);
    drop(runtime);
    environment.close().await;
}

#[tokio::test]
async fn resource_transactions_enforce_ownership_current_revision_and_idempotent_membership() {
    let Some(environment) = Environment::create().await else {
        return;
    };
    environment
        .install_plugin(
            json!({}),
            vec![account_grant("groups"), account_grant("keys")],
        )
        .await;
    let (runtime, core) = environment.runtime().await;
    let resources = gateway_admin::initialize_plugin_resources(
        environment.store.admin_ports().plugin_resources(),
        core.snapshot_control(),
    );
    let owner = owner(&environment).await;
    let account = environment.account(None).await;
    let concurrent_context = mutation();
    let (left, right) = tokio::join!(
        resources.ensure_group(
            &owner,
            "one".into(),
            group("plugin one"),
            &concurrent_context
        ),
        resources.ensure_group(
            &owner,
            "one".into(),
            group("concurrent name"),
            &concurrent_context
        )
    );
    let one = left.unwrap();
    assert_eq!(one, right.unwrap());
    let other = environment.account_group_with_account(&account).await;
    assert!(
        resources
            .ensure_group(
                &owner,
                "collision".into(),
                group(&format!("fixture {}", other.as_str())),
                &mutation()
            )
            .await
            .is_err()
    );
    assert!(
        resources
            .ensure_key(&owner, "key".into(), vec![], key(), &mutation())
            .await
            .is_err()
    );
    assert!(
        resources
            .ensure_key(
                &owner,
                "key".into(),
                vec![other.as_str().to_owned()],
                key(),
                &mutation()
            )
            .await
            .is_err()
    );
    let created = resources
        .ensure_key(&owner, "key".into(), vec!["one".into()], key(), &mutation())
        .await
        .unwrap();
    assert_eq!(created.name, "managed key");
    assert_eq!(
        created,
        resources
            .ensure_key(&owner, "key".into(), vec!["one".into()], key(), &mutation())
            .await
            .unwrap()
    );
    let change = || GroupMembersChange {
        resource_key: "one".into(),
        add: vec![account.as_str().to_owned()],
        remove: vec![],
    };
    assert_eq!(
        resources
            .change_members(&owner, change(), &mutation())
            .await
            .unwrap()
            .added,
        1
    );
    let before = revision(&environment).await;
    assert_eq!(
        resources
            .change_members(&owner, change(), &mutation())
            .await
            .unwrap()
            .added,
        0
    );
    assert_eq!(revision(&environment).await, before);
    let result = resources
        .change_members(
            &owner,
            GroupMembersChange {
                resource_key: "one".into(),
                add: vec![],
                remove: vec![account.as_str().to_owned()],
            },
            &mutation(),
        )
        .await
        .unwrap();
    assert_eq!(result.removed, 1);
    let members = environment
        .store
        .admin_ports()
        .account_groups()
        .load_account_group_members(&[
            other.clone(),
            gateway_core::routing::AccountGroupId::new(one.id).unwrap(),
        ])
        .await
        .unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].group_id, other);
    let store = environment.store.admin_ports().plugins();
    let snapshot = store.load_instances().await.unwrap();
    let mut instance = snapshot.instances[0].clone();
    instance.name = "new configuration revision".into();
    store
        .save_instance(instance, snapshot.config_revision, &mutation())
        .await
        .unwrap();
    assert!(
        resources
            .ensure_group(&owner, "stale".into(), group("stale"), &mutation())
            .await
            .is_err()
    );
    runtime.shutdown().await;
    environment.release_plugin_accounts(&runtime);
    drop(resources);
    drop(core);
    drop(runtime);
    environment.close().await;
}
