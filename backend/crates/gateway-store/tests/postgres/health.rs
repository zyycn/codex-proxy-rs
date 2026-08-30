use std::time::Duration;

use gateway_core::health::{HealthProbe as _, HealthState};
use gateway_store::PostgresHealthProbe;

use super::TestDatabase;

#[tokio::test]
async fn postgres_health_reports_a_saturated_pool_as_degraded() {
    let Some(database) = TestDatabase::create("health_saturated").await else {
        return;
    };
    let connection_1 = database.pool.acquire().await.expect("first connection");
    let connection_2 = database.pool.acquire().await.expect("second connection");
    let probe = PostgresHealthProbe::new(database.pool.clone(), 2);

    let state = probe.check().await;

    assert!(matches!(
        state,
        HealthState::Degraded(message)
            if message == "PostgreSQL pool is saturated (2/2 connections in use)"
    ));
    drop((connection_1, connection_2));
    database.close().await;
}

#[tokio::test]
async fn postgres_health_tolerates_one_transient_saturation() {
    let Some(database) = TestDatabase::create("health_transient").await else {
        return;
    };
    let connection_1 = database.pool.acquire().await.expect("first connection");
    let connection_2 = database.pool.acquire().await.expect("second connection");
    let probe = PostgresHealthProbe::new(database.pool.clone(), 2);
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(475)).await;
        drop(connection_1);
    });

    let state = probe.check().await;

    assert_eq!(state, HealthState::Healthy);
    release.await.expect("release task must not panic");
    drop(connection_2);
    database.close().await;
}
