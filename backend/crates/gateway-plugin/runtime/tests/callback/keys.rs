//! 验证插件 Key 预算回调复用宿主服务、权限与提交审计

use std::{sync::Arc, time::Duration};

use gateway_admin::{
    PluginManagementService,
    model::{
        AdminError, MutationContext,
        client_keys::ResetClientKeyBudget,
        plugin_client_keys::{PluginClientKeyFacts, PluginClientKeyListQuery, PluginClientKeyPage},
        plugin_resources::PluginResourceOwner,
        plugins::management::PluginManagementRequest,
    },
    ports::plugin_client_keys::PluginClientKeyAccess,
};
use gateway_core::{
    lifecycle::CancellationToken,
    policy::ClientApiKeyId,
    task::{WorkerContribution, WorkerRunnable},
};
use serde_json::{Value, json};
use tokio::{sync::Notify, time::timeout};

use crate::support::{environment::Environment, native};

#[tokio::test]
async fn plugin_process_manages_native_budget_through_client_key_service() {
    {
        let Some(environment) = Environment::create().await else {
            eprintln!("SKIP: plugin integration environment absent");
            return;
        };
        let key = ClientApiKeyId::new("key_budget").unwrap();
        let plaintext = format!("sk_{}", "a".repeat(43));
        environment.client_key(key.as_str(), &plaintext).await;
        environment.seed_client_key_budget(key.as_str()).await;
        let store = environment.store.admin_ports().client_keys();
        let before = store.get_client_key(&key).await.unwrap().unwrap();
        environment.install_plugin(json!({
            "management_registration":{"routes":[{"method":"POST","path":"reset","request_content_types":[],"response_content_types":["application/json"]}]},
            "data_queries":[
                {"method":"host.keys.list","query":{"limit":10}},
                {"method":"host.keys.reset_budget","query":{"client_key_id":"key_budget","period":"weekly"}},
                {"method":"host.keys.reset_budget","query":{"client_key_id":"key_budget"}},
                {"method":"host.keys.reset_budget","query":{"client_key_id":"key_budget","period":"monthly"}},
                {"method":"host.keys.reset_budget","query":{"client_key_id":"key_budget","period":"all","instance_id":"forged"}},
                {"method":"host.keys.reset_budget","query":{"client_key_id":"missing","period":"all"}},
                {"method":"host.keys.get_budget","query":{"client_key_id":"key_budget"}},
                {"method":"host.keys.update_budget_limits","query":{"client_key_id":"key_budget","weekly_limit_usd":"25.5"}},
                {"method":"host.keys.get_budget","query":{"client_key_id":"key_budget"}},
                {"method":"host.keys.update_budget_limits","query":{"client_key_id":"key_budget"}},
                {"method":"host.keys.update_budget_limits","query":{"client_key_id":"key_budget","daily_limit_usd":"-1"}},
                {"method":"host.keys.get_budget","query":{"client_key_id":"missing"}},
                {"method":"host.keys.update_budget_limits","query":{"client_key_id":"missing","weekly_limit_usd":"2"}},
                {"method":"host.keys.update_budget_limits","query":{"client_key_id":"key_budget","max_concurrency":1}},
                {"method":"host.keys.get_budget","query":{"client_key_id":"key_budget","instance_id":"forged"}}
            ]
        })).await;
        let (runtime, core) = environment.runtime().await;
        let access = gateway_admin::initialize_plugin_client_keys(
            native::admin_registry(),
            store.clone(),
            core.snapshot_control(),
        );
        runtime.bind_client_key_ports(&access).unwrap();
        let service = PluginManagementService::new(
            runtime.clone(),
            environment.store.admin_ports().plugins(),
            core.snapshots(),
        );
        let view = service.views().await.unwrap().remove(0);
        let reply = service
            .handle(
                &view.target,
                PluginManagementRequest {
                    headers: Vec::new(),
                    method: "POST".into(),
                    path: "reset".into(),
                    query: String::new(),
                    content_type: None,
                    body: vec![],
                    request_id: "budget-fixture".into(),
                },
            )
            .await
            .unwrap();
        let results: Vec<Value> = serde_json::from_slice(&reply.body).unwrap();
        let after = store.get_client_key(&key).await.unwrap().unwrap();
        let audits = environment.audit_requests("reset_budget").await;
        {
            assert_eq!(
                results[0],
                json!({"keys":[{"id":"key_budget","name":"fixture key_budget","enabled":true}],"next_cursor":null})
            );
            assert_eq!(results[1], json!({"client_key_id":"key_budget"}));
            for result in &results[2..5] {
                assert_eq!(result, &json!({"error":"invalid_input"}));
            }
            assert_eq!(results[5], json!({"error":"rejected"}));
            assert_eq!(after.budget.weekly_used_usd.canonical(), "0");
            assert_eq!(after.budget.daily_used_usd, before.budget.daily_used_usd);
            assert_eq!(
                after.budget.limits.daily_usd,
                before.budget.limits.daily_usd
            );
            assert_eq!(after.budget.limits.weekly_usd.canonical(), "25.5");
            let mut expected = json!({
                "client_key_id":"key_budget", "daily_limit_usd":"10", "weekly_limit_usd":"20",
                "daily_used_usd":"3", "weekly_used_usd":"0",
                "daily_resets_at_ms":before.budget.daily_resets_at.map(|time| chrono::DateTime::<chrono::Utc>::from(time).timestamp_millis()),
                "weekly_resets_at_ms":null,
            });
            assert_eq!(results[6], expected);
            assert_eq!(results[7], json!({"client_key_id":"key_budget"}));
            expected["weekly_limit_usd"] = json!("25.5");
            assert_eq!(results[8], expected);
            for index in [9, 10, 13, 14] {
                assert_eq!(results[index], json!({"error":"invalid_input"}));
            }
            for index in [11, 12] {
                assert_eq!(results[index], json!({"error":"rejected"}));
            }
            assert_eq!(
                environment
                    .audit_requests("update_budget_limits")
                    .await
                    .len(),
                1
            );
            assert_eq!(after.budget.daily_resets_at, before.budget.daily_resets_at);
            assert_eq!(after.budget.weekly_resets_at, None);
            assert_eq!(audits.len(), 1);
            assert!(audits[0].starts_with(&format!("plugin:{}:scope:", view.target.instance_id)));
        }
        assert_eq!(
            store
                .reveal_client_key(&key)
                .await
                .unwrap()
                .unwrap()
                .expose_for_response(),
            plaintext
        );
        assert!(
            !std::str::from_utf8(&reply.body)
                .unwrap()
                .contains(&plaintext)
        );
        drop(service);
        drop(access);
        environment.release_plugin_accounts(&runtime);
        runtime.shutdown().await;
        drop(core);
        drop(runtime);
        drop(store);
        environment.close().await;
    }
}

async fn seed_budget(environment: &Environment) -> ClientApiKeyId {
    let key = ClientApiKeyId::new("key_budget").unwrap();
    environment
        .client_key(key.as_str(), "sk-budget-fixture")
        .await;
    environment.seed_client_key_budget(key.as_str()).await;
    key
}

fn reset_queries() -> Value {
    // 重置无需先查询目录；两个基础接口由测试插件自行选择调用顺序
    json!([
        {"method":"host.keys.reset_budget","query":{"client_key_id":"key_budget","period":"weekly"}},
        {"method":"host.keys.list","query":{"limit":10}},
        {"method":"host.keys.get_budget","query":{"client_key_id":"key_budget"}},
        {"method":"host.keys.update_budget_limits","query":{"client_key_id":"key_budget","daily_limit_usd":"10"}}
    ])
}

fn assert_reset_results(results: &Value) {
    assert_eq!(results[0], json!({"client_key_id":"key_budget"}));
    assert_eq!(results[1]["keys"][0]["id"], "key_budget");
    assert_eq!(results[2]["daily_used_usd"], "3");
    assert_eq!(results[2]["weekly_used_usd"], "0");
    assert_eq!(results[3], json!({"client_key_id":"key_budget"}));
}

#[tokio::test]
async fn command_plane_resets_budget_with_only_budget_permission() {
    let Some(mut environment) = Environment::create_command().await else {
        eprintln!("SKIP: plugin integration environment absent");
        return;
    };
    let key = seed_budget(&environment).await;
    environment
        .install_plugin(json!({
            "command_registration":{"commands":[{"name":"reset","description":"预算接口测试"}]},
            "data_queries":reset_queries()
        }))
        .await;
    let (runtime, core) = environment.command_plane().await;
    let store = environment.store.admin_ports().client_keys();
    let before = store.get_client_key(&key).await.unwrap().unwrap();
    let access = gateway_admin::initialize_plugin_client_keys(
        native::admin_registry(),
        store.clone(),
        core.snapshot_control(),
    );
    runtime.bind_client_key_ports(&access).unwrap();
    let instance = environment
        .store
        .admin_ports()
        .plugins()
        .load_instances()
        .await
        .unwrap()
        .instances
        .remove(0);
    let commands = runtime.prepare_command_line().await.unwrap();
    environment.store.start_command_line_writes().unwrap();
    let reply = commands.execute(&instance.id, "reset", &[]).await.unwrap();
    assert_eq!(reply.exit_code, 0);
    assert_reset_results(&serde_json::from_str(&reply.stdout).unwrap());
    let after = store.get_client_key(&key).await.unwrap().unwrap();
    assert_eq!(after.budget.weekly_used_usd.canonical(), "0");
    assert_eq!(after.budget.daily_used_usd, before.budget.daily_used_usd);
    assert_eq!(environment.audit_requests("reset_budget").await.len(), 1);

    commands.shutdown().await;
    runtime.shutdown().await;
    environment
        .store
        .shutdown_command_line_writes()
        .await
        .unwrap();
    drop(access);
    drop(core);
    drop(runtime);
    drop(store);
    environment.close().await;
}

#[tokio::test]
async fn published_maintenance_resets_budget_with_only_budget_permission() {
    let Some(environment) = Environment::create().await else {
        eprintln!("SKIP: plugin integration environment absent");
        return;
    };
    let key = seed_budget(&environment).await;
    let marker = environment
        .directory
        .path()
        .join("budget-maintenance.jsonl");
    environment
        .install_plugin(json!({
            "maintenance_fixture":true,
            "maintenance_marker":marker,
            "data_queries":reset_queries()
        }))
        .await;
    let (runtime, core) = environment.runtime().await;
    let store = environment.store.admin_ports().client_keys();
    let before = store.get_client_key(&key).await.unwrap().unwrap();
    let access = gateway_admin::initialize_plugin_client_keys(
        native::admin_registry(),
        store.clone(),
        core.snapshot_control(),
    );
    runtime.bind_client_key_ports(&access).unwrap();
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
    let worker = tokio::spawn(async move { task.run(cancellation).await.unwrap() });
    let result: Value = timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(content) = std::fs::read_to_string(&marker)
                && let Some(result) = content
                    .lines()
                    .find_map(|line| serde_json::from_str(line).ok())
            {
                break result;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("maintenance callback completed");
    stop.cancel();
    timeout(Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap();
    assert_reset_results(&result["results"]);
    let after = store.get_client_key(&key).await.unwrap().unwrap();
    assert_eq!(after.budget.weekly_used_usd.canonical(), "0");
    assert_eq!(after.budget.daily_used_usd, before.budget.daily_used_usd);
    assert_eq!(environment.audit_requests("reset_budget").await.len(), 1);

    environment.release_plugin_accounts(&runtime);
    runtime.shutdown().await;
    drop(access);
    drop(core);
    drop(runtime);
    drop(store);
    environment.close().await;
}

struct HoldCommittedReply {
    inner: Arc<dyn PluginClientKeyAccess>,
    committed: Arc<Notify>,
}

#[async_trait::async_trait]
impl PluginClientKeyAccess for HoldCommittedReply {
    async fn facts(&self, id: &ClientApiKeyId) -> Result<PluginClientKeyFacts, AdminError> {
        self.inner.facts(id).await
    }

    async fn budget(
        &self,
        id: &ClientApiKeyId,
    ) -> Result<gateway_core::engine::budget::ClientBudgetStatus, AdminError> {
        self.inner.budget(id).await
    }
    async fn update_budget_limits(
        &self,
        owner: &PluginResourceOwner,
        command: gateway_admin::model::client_keys::UpdateClientKeyBudgetLimits,
        context: &MutationContext,
    ) -> Result<ClientApiKeyId, AdminError> {
        self.inner
            .update_budget_limits(owner, command, context)
            .await
    }

    async fn reset_budget(
        &self,
        owner: &PluginResourceOwner,
        command: ResetClientKeyBudget,
        context: &MutationContext,
    ) -> Result<ClientApiKeyId, AdminError> {
        self.inner.reset_budget(owner, command, context).await?;
        // 在真实事务提交后挡住宿主回包，稳定复现插件未收到提交结果的窗口
        self.committed.notify_one();
        std::future::pending().await
    }

    async fn list(
        &self,
        query: PluginClientKeyListQuery,
    ) -> Result<PluginClientKeyPage, AdminError> {
        self.inner.list(query).await
    }
}

#[tokio::test]
async fn losing_callback_reply_after_commit_keeps_budget_and_audit_without_replay() {
    let Some(environment) = Environment::create().await else {
        eprintln!("SKIP: plugin integration environment absent");
        return;
    };
    let key = seed_budget(&environment).await;
    let disconnect = environment.directory.path().join("disconnect-after-commit");
    let startups = environment.directory.path().join("budget-startups.jsonl");
    environment
        .install_plugin(
            json!({
                "management_registration":{"routes":[{"method":"POST","path":"reset","request_content_types":[],"response_content_types":["application/json"]}]},
                "data_queries":reset_queries(),
                "startup_marker":startups,
                "exit_after_ready_signals":[disconnect]
            }),

        )
        .await;
    let (runtime, core) = environment.runtime().await;
    let store = environment.store.admin_ports().client_keys();
    let committed = Arc::new(Notify::new());
    let access: Arc<dyn PluginClientKeyAccess> = Arc::new(HoldCommittedReply {
        inner: gateway_admin::initialize_plugin_client_keys(
            native::admin_registry(),
            store.clone(),
            core.snapshot_control(),
        ),
        committed: committed.clone(),
    });
    runtime.bind_client_key_ports(&access).unwrap();
    let service = PluginManagementService::new(
        runtime.clone(),
        environment.store.admin_ports().plugins(),
        core.snapshots(),
    );
    let view = service.views().await.unwrap().remove(0);
    let request = tokio::spawn(async move {
        service
            .handle(
                &view.target,
                PluginManagementRequest {
                    headers: Vec::new(),
                    method: "POST".into(),
                    path: "reset".into(),
                    query: String::new(),
                    content_type: None,
                    body: vec![],
                    request_id: "budget-lost-reply".into(),
                },
            )
            .await
    });
    timeout(Duration::from_secs(15), committed.notified())
        .await
        .expect("native budget transaction committed");
    assert!(!request.is_finished());
    // 只断开插件，宿主保持运行，确保连接丢失不会触发透明重放
    std::fs::write(&disconnect, b"").unwrap();
    assert!(
        timeout(Duration::from_secs(5), request)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let after = store.get_client_key(&key).await.unwrap().unwrap();
    assert_eq!(after.budget.weekly_used_usd.canonical(), "0");
    assert_eq!(environment.audit_requests("reset_budget").await.len(), 1);
    assert_eq!(
        std::fs::read_to_string(startups).unwrap().lines().count(),
        1
    );

    environment.release_plugin_accounts(&runtime);
    runtime.shutdown().await;
    drop(access);
    drop(core);
    drop(runtime);
    drop(store);
    environment.close().await;
}
