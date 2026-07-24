use chrono::{DateTime, Duration, Utc};
use gateway_store::postgres::{
    ClientAdmissionRecentRequest, ClientAdmissionRecovery, ClientAdmissionRecoveryRepository,
    ClientAdmissionRunningRequest, PgClientAdmissionRecoveryRepository,
};
use sqlx::PgPool;

use super::TestDatabase;

#[tokio::test]
async fn recovery_loads_precise_window_and_running_request_facts() {
    let Some(database) = TestDatabase::create("admission_recovery").await else {
        return;
    };
    let now = DateTime::from_timestamp_micros(Utc::now().timestamp_micros())
        .expect("current time is representable at PostgreSQL precision");
    let window_started_at = now - Duration::seconds(60);
    seed_request(
        &database.pool,
        "old-running",
        now - Duration::seconds(120),
        now + Duration::seconds(30),
        "running",
    )
    .await;
    seed_request(
        &database.pool,
        "old-complete",
        now - Duration::seconds(90),
        now - Duration::seconds(30),
        "succeeded",
    )
    .await;
    seed_request(
        &database.pool,
        "recent-complete",
        now - Duration::seconds(20),
        now + Duration::seconds(10),
        "succeeded",
    )
    .await;
    seed_request(
        &database.pool,
        "recent-running",
        now - Duration::seconds(10),
        now + Duration::seconds(40),
        "running",
    )
    .await;

    let repository = PgClientAdmissionRecoveryRepository::new(database.pool.clone());
    let actual = repository
        .load_client_admission_recovery(window_started_at)
        .await
        .expect("load precise admission recovery facts");
    let expected = vec![ClientAdmissionRecovery {
        client_api_key_ref: "key-recovery".to_owned(),
        recent_requests: vec![
            ClientAdmissionRecentRequest {
                model_request_id: "recent-complete".to_owned(),
                started_at: now - Duration::seconds(20),
            },
            ClientAdmissionRecentRequest {
                model_request_id: "recent-running".to_owned(),
                started_at: now - Duration::seconds(10),
            },
        ],
        running_requests: vec![
            ClientAdmissionRunningRequest {
                model_request_id: "old-running".to_owned(),
                deadline_at: now + Duration::seconds(30),
            },
            ClientAdmissionRunningRequest {
                model_request_id: "recent-running".to_owned(),
                deadline_at: now + Duration::seconds(40),
            },
        ],
    }];
    assert_eq!(actual, expected);

    database.close().await;
}

async fn seed_request(
    pool: &PgPool,
    id: &str,
    started_at: DateTime<Utc>,
    deadline_at: DateTime<Utc>,
    outcome: &str,
) {
    let completed_at = (outcome != "running").then_some(started_at + Duration::seconds(1));
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, config_revision, protocol, operation, endpoint,
           client_transport, requested_model_id, outcome,
           started_at, deadline_at, completed_at
         ) values (
           $1, 'key-recovery', 1, 'openai', 'responses', '/v1/responses',
           'http_sse', 'coding', $2, $3, $4, $5
         )",
    )
    .bind(id)
    .bind(outcome)
    .bind(started_at)
    .bind(deadline_at)
    .bind(completed_at)
    .execute(pool)
    .await
    .expect("seed model request recovery fact");
}
