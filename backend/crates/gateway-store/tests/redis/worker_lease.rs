//! Worker leader lease 通过 StoreBundle 的中立能力进行集成验证。

use std::collections::BTreeSet;
use std::time::Duration;

use gateway_core::task::{WorkerId, WorkerKind, WorkerLeaseAcquisition, WorkerLeaseRequest};
use gateway_store::{StoreConfig, initialize};
use uuid::Uuid;

#[tokio::test]
async fn store_bundle_worker_plan_and_leader_lease_are_single_use_and_fenced() {
    let (Some(database_url), Some(redis_url)) = (
        std::env::var("CPR_TEST_DATABASE_URL").ok(),
        std::env::var("CPR_TEST_REDIS_URL").ok(),
    ) else {
        return;
    };
    let config = store_config(&database_url, &redis_url);
    let mut first = initialize(config.clone())
        .await
        .expect("first Store bundle");
    let second = initialize(config).await.expect("second Store bundle");

    let kinds = first
        .take_worker_contributions()
        .into_iter()
        .map(|contribution| contribution.kind())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        kinds,
        BTreeSet::from([
            WorkerKind::NativeClaimRecovery,
            WorkerKind::StaleModelRequestRecovery,
            WorkerKind::Retention,
            WorkerKind::OpsFlush,
        ])
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
}

fn store_config(database_url: &str, redis_url: &str) -> StoreConfig {
    let (database_url, database_password) = split_connection_url(database_url);
    let (redis_url, redis_password) = split_connection_url(redis_url);
    serde_json::from_value(serde_json::json!({
        "database": { "url": database_url, "password": database_password },
        "redis": { "url": redis_url, "password": redis_password },
    }))
    .expect("test Store config")
}

fn split_connection_url(value: &str) -> (String, String) {
    let mut url = url::Url::parse(value).expect("test connection URL");
    let password = url.password().expect("test connection password").to_owned();
    url.set_password(None)
        .expect("connection URL supports credentials");
    (url.to_string(), password)
}
