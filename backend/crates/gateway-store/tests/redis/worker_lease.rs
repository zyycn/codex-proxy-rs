//! Worker leader lease 通过 StoreBundle 的中立能力进行集成验证

use std::collections::BTreeSet;
use std::str::FromStr;
use std::time::Duration;

use gateway_core::task::{WorkerId, WorkerKind, WorkerLeaseAcquisition, WorkerLeaseRequest};
use gateway_store::{StoreConfig, initialize};
use sqlx::{
    ConnectOptions as _, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

#[tokio::test]
async fn store_replicas_share_peer_identity_and_fence_the_worker_lease() {
    let (Some(database_url), Some(redis_url)) = (
        crate::support::test_env("CPR_TEST_DATABASE_URL"),
        crate::support::test_env("CPR_TEST_REDIS_URL"),
    ) else {
        return;
    };
    let database = TestDatabase::create(&database_url).await;
    let runtime_data = tempfile::tempdir().expect("Store runtime data");
    let mut config = store_config(&database.url, &redis_url);
    config
        .resolve_and_validate(runtime_data.path())
        .expect("resolved Store config");
    let mut first = initialize(config.clone())
        .await
        .expect("first Store bundle");
    let second = initialize(config).await.expect("second Store bundle");

    assert_eq!(
        first.peer_routing_id().await.unwrap(),
        second.peer_routing_id().await.unwrap()
    );
    assert!(runtime_data.path().join("backup-staging").is_dir());

    let kinds = first
        .take_worker_contributions()
        .into_iter()
        .map(|contribution| contribution.kind())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        kinds,
        BTreeSet::from([WorkerKind::StaleModelRequestRecovery, WorkerKind::OpsFlush,])
    );
    assert!(first.take_worker_contributions().is_empty());

    let worker = WorkerId::try_new(
        WorkerKind::Retention,
        format!("test-{}", Uuid::new_v4().simple()),
    )
    .expect("worker ID");
    let request =
        WorkerLeaseRequest::try_new(worker, Duration::from_secs(5)).expect("worker lease request");
    let first_port = first.worker_leader_lease();
    let second_port = second.worker_leader_lease();
    let mut first_guard = match first_port
        .try_acquire(request.clone())
        .await
        .expect("first acquisition")
    {
        WorkerLeaseAcquisition::Acquired(guard) => guard,
        WorkerLeaseAcquisition::Busy { .. } => panic!("fresh worker lease must be available"),
    };
    let first_token = first_guard.fencing_token();
    assert!(matches!(
        second_port
            .try_acquire(request.clone())
            .await
            .expect("contended acquisition"),
        WorkerLeaseAcquisition::Busy { .. }
    ));
    first_guard.renew().await.expect("leader lease renewal");
    first_guard.release().await.expect("leader lease release");

    let second_guard = match second_port
        .try_acquire(request)
        .await
        .expect("acquisition after release")
    {
        WorkerLeaseAcquisition::Acquired(guard) => guard,
        WorkerLeaseAcquisition::Busy { .. } => panic!("released worker lease must be reusable"),
    };
    assert!(second_guard.fencing_token() > first_token);
    second_guard.release().await.expect("second lease release");

    drop(first);
    drop(second);
    database.close().await;
}

struct TestDatabase {
    admin: PgPool,
    name: String,
    url: String,
}

impl TestDatabase {
    async fn create(database_url: &str) -> Self {
        let name = format!("cpr_store_worker_{}", Uuid::new_v4().simple());
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(database_url)
            .await
            .expect("connect test PostgreSQL");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create database \"{name}\"")))
            .execute(&admin)
            .await
            .expect("create worker test database");
        let url = PgConnectOptions::from_str(database_url)
            .expect("parse test PostgreSQL URL")
            .database(&name)
            .to_url_lossy()
            .to_string();
        Self { admin, name, url }
    }

    async fn close(self) {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "drop database \"{}\" with (force)",
            self.name
        )))
        .execute(&self.admin)
        .await
        .expect("drop worker test database");
        self.admin.close().await;
    }
}

fn store_config(database_url: &str, redis_url: &str) -> StoreConfig {
    serde_json::from_value(serde_json::json!({
        "database": crate::support::config_connection(url::Url::parse(database_url).unwrap()),
        "redis": crate::support::config_connection(url::Url::parse(redis_url).unwrap()),
    }))
    .expect("test Store config")
}
