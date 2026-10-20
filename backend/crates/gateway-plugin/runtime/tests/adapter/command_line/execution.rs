//! 验证插件命令复用宿主身份、计量、账号事务与审计边界

use gateway_core::policy::{ClientApiKeyId, RateLimits};
use gateway_plugin_runtime::PluginCommandError;
use serde_json::json;

use crate::support::environment::Environment;

#[tokio::test]
async fn model_requests_have_a_body_limit_independent_of_the_response_buffer() {
    use gateway_plugin_sdk::call::model::request_payload_limit;

    for stream in [false, true] {
        let Some(mut environment) = Environment::create_command().await else {
            eprintln!("SKIP: plugin integration environment absent");
            return;
        };
        let account = environment.account(None).await;
        let key_id = format!("key_{}", uuid::Uuid::new_v4().simple());
        environment
            .client_key(&key_id, "sk-large-body-fixture")
            .await;
        let body_path = environment.directory.path().join("model-body.json");
        let mut body = serde_json::to_vec(&json!({
            "model":crate::support::native::MODEL,
            "input":"x".repeat(2 * 1024 * 1024),
        }))
        .unwrap();
        let raw_limit = 2 * 1024 * 1024;
        body.resize(request_payload_limit(raw_limit), b' ');
        std::fs::write(&body_path, &body).unwrap();
        let marker = environment.directory.path().join("model-results.jsonl");
        environment
            .install_plugin(json!({
                "plugin_id":"test.large-model-request",
                "command_registration":{"commands":[{"name":"run","description":"执行模型"}]},
                "command_nested_model_fixture":{
                    "request":{
                        "client_key_id":key_id,
                        "model":crate::support::native::MODEL,
                        "protocol":"openai",
                        "operation":"generate",
                        "provider":"openai",
                        "account_id":account.as_str(),
                    },
                    "body_file":body_path,
                    "stream":stream,
                    "expect_error":true,
                },
                "command_nested_model_marker":marker,
                "command_result":{"stdout":"ok\n","stderr":"","exit_code":0},
            }))
            .await;
        let (runtime, core) = environment.runtime().await;
        let snapshots = core.snapshots();
        let configure = |bytes: usize| {
            let snapshot = snapshots.acquire().unwrap();
            let settings = snapshot
                .settings()
                .clone()
                .with_responses_max_decompressed_body_bytes(bytes as u64);
            let updated = gateway_core::routing::request_settings::RequestSettings::new(snapshot)
                .replace(settings, "fixture")
                .unwrap();
            snapshots.publish((*updated.snapshot()).clone());
        };
        configure(raw_limit);
        environment.store.start_command_line_writes().unwrap();
        let instances = environment
            .store
            .admin_ports()
            .plugins()
            .load_instances()
            .await
            .unwrap();
        let commands = runtime.prepare_command_line().await.unwrap();
        let instance = &instances.instances[0];
        commands.execute(&instance.id, "run", &[]).await.unwrap();
        environment.wait_for_bound_model_requests(&key_id, 1).await;
        let first: serde_json::Value = serde_json::from_str(
            std::fs::read_to_string(&marker)
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        assert!(first["request_id"].is_string(), "{first}");
        assert!(first["events"].as_u64().unwrap() > 0);

        // 合法 JSON 仅多一个空白字节，超限必须在进入 Core 和账本前拒绝
        let mut oversized = body.clone();
        oversized.push(b'\n');
        std::fs::write(&body_path, oversized).unwrap();
        commands.execute(&instance.id, "run", &[]).await.unwrap();
        let records = std::fs::read_to_string(&marker).unwrap();
        let rejected: serde_json::Value =
            serde_json::from_str(records.lines().last().unwrap()).unwrap();
        assert_eq!(rejected["error"], "invalid_input");
        assert_eq!(rejected["http_status"], 413);
        assert_eq!(environment.bound_model_requests(&key_id).await.len(), 1);

        // 不重新准备插件实例：发布更大的配置后，同一个请求必须恢复成功
        configure(raw_limit + 1);
        commands.execute(&instance.id, "run", &[]).await.unwrap();
        environment.wait_for_bound_model_requests(&key_id, 2).await;
        commands.shutdown().await;
        environment
            .store
            .shutdown_command_line_writes()
            .await
            .unwrap();
        drop(core);
        drop(runtime);
        environment.close().await;
    }
}

#[tokio::test]
async fn command_selects_key_at_call_time_and_uses_normal_model_ledger() {
    let Some(mut environment) = Environment::create_command().await else {
        eprintln!("SKIP: plugin integration environment absent");
        return;
    };
    let account = environment.account(None).await;
    let group_id = environment.account_group_with_account(&account).await;
    let key_id = format!("key_{}", uuid::Uuid::new_v4().simple());
    environment
        .client_key_with_limits_and_groups(
            &key_id,
            "sk-command-bound-fixture",
            RateLimits {
                max_concurrency: 1,
                requests_per_minute: 0,
            },
            vec![group_id.clone()],
        )
        .await;
    let marker = environment
        .directory
        .path()
        .join("command-bound-model.jsonl");
    let command_marker = environment.directory.path().join("command-bound.jsonl");
    environment
        .install_plugin(json!({
            "plugin_id":"test.command-source",
            "command_registration":{"commands":[
                {"name":"run","description":"执行模型"},
                {"name":"fail","description":"执行后失败"}
            ]},
            "command_nested_model_fixture":{
                "request":{
                    "client_key_id":key_id,
                    "model":crate::support::native::MODEL,
                    "protocol":"openai",
                    "operation":"generate",
                    "provider":"openai",
                    "account_id":account.as_str()
                },
                "body":{"model":crate::support::native::MODEL,"input":"bound command"}
            },
            "command_nested_model_marker":marker,
            "command_marker":command_marker,
            "command_error_after_nested_model":"fail",
            "command_result":{"stdout":"ok\n","stderr":"","exit_code":0}
        }))
        .await;
    let (runtime, core) = environment.command_plane().await;
    let snapshot = environment
        .store
        .admin_ports()
        .plugins()
        .load_instances()
        .await
        .unwrap();
    let instance = snapshot
        .instances
        .iter()
        .find(|instance| instance.configuration.get("command_registration").is_some())
        .unwrap();
    let commands = runtime.prepare_command_line().await.unwrap();
    environment.store.start_command_line_writes().unwrap();
    let output = commands.execute(&instance.id, "run", &[]).await.unwrap();
    assert_eq!(output.stdout, "ok\n");
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);
    assert_eq!(
        environment.wait_for_bound_model_requests(&key_id, 1).await,
        vec![("plugin_bound_model".into(), instance.id.clone())]
    );
    let second = commands.execute(&instance.id, "run", &[]).await.unwrap();
    assert_eq!(second.stdout, "ok\n", "the first Key slot must be released");
    assert_eq!(
        environment.wait_for_bound_model_requests(&key_id, 2).await,
        vec![
            ("plugin_bound_model".into(), instance.id.clone()),
            ("plugin_bound_model".into(), instance.id.clone()),
        ]
    );

    environment
        .set_account_group_enabled(group_id.clone(), false)
        .await;
    assert!(
        environment
            .store
            .admin_ports()
            .client_keys()
            .get_client_key(&ClientApiKeyId::new(key_id.clone()).unwrap())
            .await
            .unwrap()
            .unwrap()
            .enabled,
        "scope revocation must not disable the bound Key itself"
    );
    let error = commands
        .execute(&instance.id, "run", &[])
        .await
        .err()
        .unwrap();
    assert!(matches!(error, PluginCommandError::Incomplete { .. }));
    assert_eq!(
        std::fs::read_to_string(command_marker)
            .unwrap()
            .lines()
            .count(),
        3,
        "scope revocation is enforced by the host callback, not hidden from the plugin RPC"
    );
    let revoked = std::fs::read_to_string(&marker).unwrap();
    let revoked: serde_json::Value = serde_json::from_str(revoked.lines().last().unwrap()).unwrap();
    assert_eq!(revoked["error"], "rejected");
    assert_eq!(environment.bound_model_requests(&key_id).await.len(), 2);

    environment.set_account_group_enabled(group_id, true).await;
    let error = commands
        .execute(&instance.id, "fail", &[])
        .await
        .err()
        .unwrap();
    assert!(matches!(error, PluginCommandError::Incomplete { .. }));

    commands.shutdown().await;
    environment
        .store
        .shutdown_command_line_writes()
        .await
        .unwrap();
    assert_eq!(
        environment.bound_model_requests(&key_id).await.len(),
        3,
        "an error exit must drain the completed child model ledger"
    );
    drop(core);
    drop(runtime);
    environment.close().await;
}

#[tokio::test]
async fn command_accounts_use_admin_cas_audit_and_report_partial_commit_without_replay() {
    let Some(environment) = Environment::create().await else {
        eprintln!("SKIP: plugin integration environment absent");
        return;
    };
    let existing = environment.account(None).await;
    let facts = json!({"name":"CLI created account","authentication_kind":"api_key","material":{"key":"cli-test-only"}});
    let config = json!({
        "command_registration":{"commands":[{"name":"login","description":"提交账号"}]},
        "command_registration_probe":true,
        "command_result":{"stdout":"saved\n","stderr":"","exit_code":0,"accounts":[
            {"action":"create","provider_id":"openai","facts":facts},
            {"action":"replace","account_id":existing.as_str(),"credential_revision":999,"facts":facts}
        ]}
    });
    let (runtime, core) = environment.plugin(config).await;
    let admin = environment.bind_admin_accounts(&runtime, &core).await;
    let snapshot = environment
        .store
        .admin_ports()
        .plugins()
        .load_instances()
        .await
        .unwrap();
    let id = &snapshot.instances[0].id;
    let commands = runtime.prepare_command_line().await.unwrap();
    assert!(commands.help(Some(id), Some("login")).is_ok());
    assert_eq!(
        environment
            .store
            .provider_ports()
            .accounts()
            .list_accounts()
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        environment
            .audit_requests("import_document")
            .await
            .is_empty()
    );
    let error = commands.execute(id, "login", &[]).await.err().unwrap();
    assert!(matches!(
        error,
        PluginCommandError::Incomplete {
            saved_accounts: 1,
            ..
        }
    ));
    let accounts = environment
        .store
        .provider_ports()
        .accounts()
        .list_accounts()
        .await
        .unwrap();
    assert_eq!(accounts.len(), 2);
    assert_eq!(
        accounts
            .iter()
            .find(|account| account.id() == &existing)
            .unwrap()
            .revision()
            .get(),
        1
    );
    let created = accounts
        .iter()
        .find(|account| account.id() != &existing)
        .unwrap();
    assert!(created.id().as_str().starts_with("acct_"));
    assert_eq!(created.name(), "CLI created account");
    let audits = environment.audit_requests("import_document").await;
    assert_eq!(audits.len(), 1);
    assert!(audits[0].contains(":scope:"));
    assert!(!error.to_string().contains("cli-test-only"));
    drop(commands);
    drop(admin);
    drop(core);
    drop(runtime);
    environment.close().await;
}

#[tokio::test]
async fn command_accounts_domain_does_not_need_separate_login_or_write_grants() {
    let Some(environment) = Environment::create().await else {
        eprintln!("SKIP: plugin integration environment absent");
        return;
    };
    let config = json!({
        "command_registration":{"commands":[{"name":"login","description":"提交账号"}]},
        "command_result":{"stdout":"","stderr":"","exit_code":0,"accounts":[
            {"action":"create","provider_id":"openai","facts":{"name":"CLI created","authentication_kind":"api_key","material":{"key":"cli-test-only"}}}
        ]}
    });
    let (runtime, core) = environment.plugin(config).await;
    let admin = environment.bind_admin_accounts(&runtime, &core).await;
    let snapshot = environment
        .store
        .admin_ports()
        .plugins()
        .load_instances()
        .await
        .unwrap();
    let commands = runtime.prepare_command_line().await.unwrap();
    let output = commands
        .execute(&snapshot.instances[0].id, "login", &[])
        .await
        .unwrap();
    assert_eq!(output.saved_accounts, 1);
    assert_eq!(
        environment
            .store
            .provider_ports()
            .accounts()
            .list_accounts()
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(environment.audit_requests("import_document").await.len(), 1);
    drop(commands);
    drop(admin);
    drop(core);
    drop(runtime);
    environment.close().await;
}

#[tokio::test]
async fn bound_model_body_budget_stays_frozen_until_the_parent_call_ends() {
    use gateway_plugin_sdk::call::model::request_payload_limit;

    for stream in [false, true] {
        for growing in [false, true] {
            let Some(mut environment) = Environment::create_command().await else {
                eprintln!("SKIP: plugin integration environment absent");
                return;
            };
            let account = environment.account(None).await;
            let key_id = format!("key_{}", uuid::Uuid::new_v4().simple());
            environment
                .client_key(&key_id, "sk-frozen-body-fixture")
                .await;
            let old_limit = if growing { 64 * 1024 } else { 128 * 1024 };
            let new_limit = if growing { 128 * 1024 } else { 64 * 1024 };
            let body_path = environment.directory.path().join("model-body.json");
            let body = json!({"model":crate::support::native::MODEL,"input":"hello"});
            let mut large_body = serde_json::to_vec(&body).unwrap();
            large_body.resize(
                request_payload_limit(old_limit) + usize::from(growing),
                b' ',
            );
            std::fs::write(&body_path, large_body).unwrap();
            let marker = environment.directory.path().join("model-results.jsonl");
            let gate = environment.directory.path().join("resume-model");
            let request = json!({
                "client_key_id":key_id,"model":crate::support::native::MODEL,
                "protocol":"openai","operation":"generate","provider":"openai",
                "account_id":account.as_str(),
            });
            environment
                .install_plugin(json!({
                    "plugin_id":"test.frozen-model-request",
                    "command_registration":{"commands":[{"name":"run","description":"执行模型"}]},
                    "command_nested_model_fixture":[
                        {"request":request,"body":body,"stream":stream},
                        {"request":request,"body_file":body_path,"stream":stream,
                            "expect_error":true,"wait_for_file":gate},
                    ],
                    "command_nested_model_marker":marker,
                    "command_result":{"stdout":"ok\n","stderr":"","exit_code":0},
                }))
                .await;
            let (runtime, core) = environment.runtime().await;
            let snapshots = core.snapshots();
            let configure = |bytes: usize| {
                let snapshot = snapshots.acquire().unwrap();
                let settings = snapshot
                    .settings()
                    .clone()
                    .with_responses_max_decompressed_body_bytes(bytes as u64);
                let updated =
                    gateway_core::routing::request_settings::RequestSettings::new(snapshot)
                        .replace(settings, "fixture")
                        .unwrap();
                snapshots.publish((*updated.snapshot()).clone());
            };
            configure(old_limit);
            environment.store.start_command_line_writes().unwrap();
            let instances = environment
                .store
                .admin_ports()
                .plugins()
                .load_instances()
                .await
                .unwrap();
            let commands = runtime.prepare_command_line().await.unwrap();
            let instance = &instances.instances[0];
            let update = async {
                tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    while std::fs::read_to_string(&marker)
                        .unwrap_or_default()
                        .lines()
                        .count()
                        < 1
                    {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
                configure(new_limit);
                std::fs::write(&gate, b"resume").unwrap();
            };
            let (result, ()) = tokio::join!(commands.execute(&instance.id, "run", &[]), update);
            result.unwrap();
            let expected = if growing { 1 } else { 2 };
            environment
                .wait_for_bound_model_requests(&key_id, expected)
                .await;
            assert_eq!(
                environment.bound_model_requests(&key_id).await.len(),
                expected
            );

            // 父调用结束后重新绑定，下一次调用才使用新发布的预算
            commands.execute(&instance.id, "run", &[]).await.unwrap();
            environment.wait_for_bound_model_requests(&key_id, 3).await;
            let records = std::fs::read_to_string(&marker)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(records.len(), 4);
            for (index, rejected) in [(1, growing), (3, !growing)] {
                if rejected {
                    assert_eq!(records[index]["http_status"], 413, "{records:?}");
                    assert_eq!(records[index]["error"], "invalid_input");
                } else {
                    assert!(records[index]["request_id"].is_string(), "{records:?}");
                }
            }
            assert_eq!(environment.bound_model_requests(&key_id).await.len(), 3);
            commands.shutdown().await;
            environment
                .store
                .shutdown_command_line_writes()
                .await
                .unwrap();
            drop(core);
            drop(runtime);
            environment.close().await;
        }
    }
}
