//! 验证 Store 认证、凭据编码、只读写入边界与账号租约关闭清理

use gateway_core::{
    account::{
        CredentialRevision, NewProviderAccount, PlaintextCredential, ProviderAccount,
        ProviderAccountId,
    },
    routing::ProviderKind,
};
use gateway_store::StoreConfig;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;

use crate::support::config_connection;

#[tokio::test]
async fn shutdown_releases_owned_account_pools_and_preserves_other_instances() {
    use gateway_core::lifecycle::{CancellationToken, Deadline};
    use gateway_core::provider_ports::{
        ProviderConcurrencyPool, ProviderLeaseAcquisition, ProviderLeaseRequest,
        ProviderSchedulingLeaseRequest,
    };
    use gateway_core::task::{WorkerContribution, WorkerDaemonContext, WorkerRunnable};
    use std::{num::NonZeroU32, time::Duration};

    let (Some(database), Some(redis)) = (
        crate::support::test_env("CPR_TEST_DATABASE_URL"),
        crate::support::test_env("CPR_TEST_REDIS_URL"),
    ) else {
        return;
    };
    let mut config: StoreConfig = serde_json::from_value(json!({
        "database": config_connection(url::Url::parse(&database).unwrap()),
        "redis": config_connection(url::Url::parse(&redis).unwrap()),
    }))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    config.resolve_and_validate(directory.path()).unwrap();
    // 只使用真实 Bundle 的租约端口与关闭任务，不需要创建业务账号或启动其他 Worker
    let mut first = gateway_store::initialize_read_only(config.clone())
        .await
        .unwrap();
    let mut second = gateway_store::initialize_read_only(config).await.unwrap();
    let first_port = first.provider_ports().leases();
    let second_port = second.provider_ports().leases();
    let account =
        ProviderAccountId::new(format!("acct_shutdown_{}", uuid::Uuid::new_v4().simple())).unwrap();
    let mut first_requests = Vec::new();
    let mut first_guards = Vec::new();
    let mut second_requests = Vec::new();
    let mut second_guards = Vec::new();
    for pool in [
        ProviderConcurrencyPool::Shared,
        ProviderConcurrencyPool::Reserved,
    ] {
        let request = ProviderSchedulingLeaseRequest::new(
            ProviderKind::new("openai").unwrap(),
            account.clone(),
            CredentialRevision::new(1).unwrap(),
            NonZeroU32::new(2).unwrap(),
            Deadline::default(),
        )
        .with_concurrency_pool(pool);
        let ProviderLeaseAcquisition::Acquired(first_guard) = first_port
            .try_acquire(ProviderLeaseRequest::Scheduling(request.clone()))
            .await
            .unwrap()
        else {
            panic!("first instance must acquire a slot")
        };
        let other = request.clone().with_cancellation(CancellationToken::new());
        let ProviderLeaseAcquisition::Acquired(second_guard) = second_port
            .try_acquire(ProviderLeaseRequest::Scheduling(other.clone()))
            .await
            .unwrap()
        else {
            panic!("second instance must acquire a slot")
        };
        assert!(matches!(
            second_port
                .try_acquire(ProviderLeaseRequest::Scheduling(other.clone()))
                .await
                .unwrap(),
            ProviderLeaseAcquisition::Busy { .. }
        ));
        first_requests.push(request);
        second_requests.push(other);
        first_guards.push(first_guard);
        second_guards.push(second_guard);
    }
    assert_eq!(
        second_port
            .account_in_flight(std::slice::from_ref(&account))
            .await
            .unwrap()[&account],
        2
    );

    for (index, store) in [&mut first, &mut second].into_iter().enumerate() {
        let cleanup = store
            .take_worker_contributions()
            .into_iter()
            .find_map(|contribution| {
                let WorkerContribution::Registration(registration) = contribution else {
                    return None;
                };
                if registration.id.owner() != "redis_account_leases" {
                    return None;
                }
                let WorkerRunnable::Daemon { task, .. } = registration.runnable else {
                    panic!("cleanup is a daemon")
                };
                Some(task)
            })
            .expect("account lease shutdown worker must be registered");
        let stopping = CancellationToken::new();
        stopping.cancel();
        tokio::time::timeout(
            Duration::from_secs(3),
            cleanup.run(WorkerDaemonContext::new(stopping)),
        )
        .await
        .unwrap()
        .unwrap();
        if index == 0 {
            assert_eq!(
                second_port
                    .account_in_flight(std::slice::from_ref(&account))
                    .await
                    .unwrap()[&account],
                1
            );
            for (first_request, other) in first_requests.iter().zip(&second_requests) {
                assert!(first_request.cancellation().is_cancelled());
                assert!(!other.cancellation().is_cancelled());
                assert!(
                    first_port
                        .try_acquire(ProviderLeaseRequest::Scheduling(first_request.clone()))
                        .await
                        .is_err()
                );
                let replacement = second_port
                    .try_acquire(ProviderLeaseRequest::Scheduling(other.clone()))
                    .await
                    .unwrap();
                assert!(matches!(
                    &replacement,
                    ProviderLeaseAcquisition::Acquired(_)
                ));
                assert!(matches!(
                    second_port
                        .try_acquire(ProviderLeaseRequest::Scheduling(other.clone()))
                        .await
                        .unwrap(),
                    ProviderLeaseAcquisition::Busy { .. }
                ));
                drop(replacement);
            }
        }
    }
    assert!(
        second_requests
            .iter()
            .all(|request| request.cancellation().is_cancelled())
    );
    assert_eq!(
        second_port
            .account_in_flight(std::slice::from_ref(&account))
            .await
            .unwrap()[&account],
        0
    );
    drop((first_guards, second_guards));
}

#[tokio::test]
async fn inspection_bundle_does_not_migrate_and_rejects_business_writes() {
    let (Some(database), Some(redis)) = (
        crate::support::test_env("CPR_TEST_DATABASE_URL"),
        crate::support::test_env("CPR_TEST_REDIS_URL"),
    ) else {
        return;
    };
    let schema = format!("cpr_inspection_{}", uuid::Uuid::new_v4().simple());
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database)
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create schema {schema}")))
        .execute(&admin)
        .await
        .unwrap();
    let mut database = url::Url::parse(&database).unwrap();
    database
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let mut config: StoreConfig = serde_json::from_value(json!({
        "database":config_connection(database), "redis":config_connection(url::Url::parse(&redis).unwrap()),
    })).unwrap();
    let directory = tempfile::tempdir().unwrap();
    config.resolve_and_validate(directory.path()).unwrap();
    let inspection = gateway_store::initialize_read_only(config.clone())
        .await
        .unwrap();
    assert!(
        inspection
            .admin_ports()
            .plugins()
            .load_instances()
            .await
            .is_err()
    );
    let tables: i64 =
        sqlx::query_scalar("select count(*) from information_schema.tables where table_schema=$1")
            .bind(&schema)
            .fetch_one(&admin)
            .await
            .unwrap();
    assert_eq!(tables, 0, "只读帮助不能在空库创建迁移表或业务表");
    drop(inspection);
    let writable = gateway_store::initialize(config.clone()).await.unwrap();
    let inspection = gateway_store::initialize_read_only(config).await.unwrap();
    let create = |id: &str| NewProviderAccount {
        account: ProviderAccount::new(
            ProviderAccountId::new(id.to_owned()).unwrap(),
            ProviderKind::new("example").unwrap(),
            "inspection test".into(),
            None,
            "api_key".into(),
            CredentialRevision::new(1).unwrap(),
            None,
        ),
        credential: PlaintextCredential::new(
            json!({"key":"test-only"}).as_object().unwrap().clone(),
        ),
        model_access: None,
    };
    writable
        .provider_ports()
        .accounts()
        .create_account(create("acct_writable"))
        .await
        .unwrap();
    assert!(
        inspection
            .provider_ports()
            .accounts()
            .create_account(create("acct_readonly"))
            .await
            .is_err()
    );
    let accounts = inspection
        .provider_ports()
        .accounts()
        .list_accounts()
        .await
        .unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].id().as_str(), "acct_writable");
    drop(inspection);
    drop(writable);
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("drop schema {schema} cascade")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[tokio::test]
async fn store_bundle_authenticates_configured_and_redis_acl_credentials() {
    let (Some(database), Some(redis_url)) = (
        crate::support::test_env("CPR_TEST_DATABASE_URL"),
        crate::support::test_env("CPR_TEST_REDIS_URL"),
    ) else {
        return;
    };
    let document = json!({
        "database": config_connection(url::Url::parse(&database).unwrap()),
        "redis": config_connection(url::Url::parse(&redis_url).unwrap()),
    });
    let mut admin = redis::Client::open(redis_url.as_str())
        .unwrap()
        .get_connection_manager()
        .await
        .unwrap();
    let username = format!("cpr_password_{}", uuid::Uuid::new_v4().simple());
    let password = "Managed%41%25%FF@:/?#%&+$'\"\\ secret9!";
    redis::cmd("ACL")
        .arg("SETUSER")
        .arg(&username)
        .arg("on")
        .arg(format!(">{password}"))
        .arg("~*")
        .arg("&*")
        .arg("+@all")
        .query_async::<()>(&mut admin)
        .await
        .unwrap();
    let mut acl_url = url::Url::parse(&redis_url).unwrap();
    acl_url.set_username(&username).unwrap();
    acl_url.set_password(None).unwrap();
    let mut acl_document = document.clone();
    acl_document["redis"] = json!({"url": acl_url.as_str(), "password": password});
    let directory = tempfile::tempdir().unwrap();
    let mut results = Vec::new();
    // 同时走 Store 的配置解析、URL 构造与真实连接路径，断言前先清理临时 ACL 用户
    for document in [document, acl_document] {
        let mut config: StoreConfig = serde_json::from_value(document).unwrap();
        let result = async {
            config.resolve_and_validate(directory.path())?;
            let bundle = gateway_store::initialize_read_only(config).await?;
            let mut states = Vec::new();
            for probe in bundle.health_probes() {
                states.push(probe.check().await);
            }
            Ok::<_, gateway_store::StoreError>(states)
        }
        .await;
        results.push(result);
    }
    redis::cmd("ACL")
        .arg("DELUSER")
        .arg(&username)
        .query_async::<i64>(&mut admin)
        .await
        .unwrap();
    for result in results {
        let states = result.expect("Store credentials authenticate without double encoding");
        assert!(!states.is_empty());
        assert!(
            states
                .iter()
                .all(|state| *state == gateway_core::health::HealthState::Healthy)
        );
    }
}

#[tokio::test]
async fn store_bundle_authenticates_postgres_password_with_percent_sequences() {
    let (Some(database), Some(redis_url)) = (
        crate::support::test_env("CPR_TEST_DATABASE_URL"),
        crate::support::test_env("CPR_TEST_REDIS_URL"),
    ) else {
        return;
    };
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database)
        .await
        .unwrap();
    let username = format!("cpr_password_{}", uuid::Uuid::new_v4().simple());
    let password = "Managed%41%25%FFPassword9!";
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE ROLE {username} LOGIN PASSWORD '{password}'"
    )))
    .execute(&admin)
    .await
    .unwrap();
    let mut database_url = url::Url::parse(&database).unwrap();
    database_url.set_username(&username).unwrap();
    database_url.set_password(None).unwrap();
    let mut config: StoreConfig = serde_json::from_value(json!({
        "database": {"url": database_url.as_str(), "password": password},
        "redis": config_connection(url::Url::parse(&redis_url).unwrap()),
    }))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let result = async {
        config.resolve_and_validate(directory.path())?;
        let bundle = gateway_store::initialize_read_only(config).await?;
        let mut states = Vec::new();
        for probe in bundle.health_probes() {
            states.push(probe.check().await);
        }
        Ok::<_, gateway_store::StoreError>(states)
    }
    .await;
    // 连接失败时也先清理临时角色，避免回归断言留下测试账号
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP ROLE {username}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let states = result.expect("PostgreSQL password percent sequences remain literal");
    assert!(!states.is_empty());
    assert!(
        states
            .iter()
            .all(|state| *state == gateway_core::health::HealthState::Healthy)
    );
}
