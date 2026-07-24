mod client_admission;
mod credential_cooldown;
mod credential_leases;
mod credential_state;
mod oauth_pending;
mod provider_circuit;
mod provider_session_affinity;
mod worker_lease;

use chrono::Utc;
use gateway_store::redis::{
    AdminAuthStateRepository, AdminSessionRecord, RedisAdminAuthStateRepository,
};
use redis::aio::ConnectionManager;
use uuid::Uuid;

#[test]
fn admin_auth_state_rejects_invalid_ttl_boundaries() {
    let session = AdminSessionRecord {
        admin_user_id: "admin".to_owned(),
        expires_at: Utc::now() - chrono::Duration::seconds(1),
    };
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let Some((repository, _connection, _namespace)) = runtime.block_on(admin_auth_repository())
    else {
        return;
    };
    assert!(
        runtime
            .block_on(repository.store_admin_session("expired-session", &session))
            .is_err()
    );
    assert!(
        runtime
            .block_on(repository.record_login_failure("source", 0, 60))
            .is_err()
    );
    assert!(
        runtime
            .block_on(repository.record_login_failure("source", 5, 0))
            .is_err()
    );
}

#[tokio::test]
async fn admin_auth_state_keeps_fixed_ttl_and_atomic_failure_window() {
    let Some((repository, mut connection, namespace)) = admin_auth_repository().await else {
        return;
    };
    let session_id = "session-real-secret-id";
    let source = "203.0.113.7";
    let password = "must-never-enter-redis";
    let admin_api_key = "admin-must-never-enter-redis";
    let session = AdminSessionRecord {
        admin_user_id: "default-admin".to_owned(),
        expires_at: Utc::now() + chrono::Duration::seconds(60),
    };

    repository
        .store_admin_session(session_id, &session)
        .await
        .expect("store session");
    assert_eq!(
        repository
            .load_admin_session(session_id)
            .await
            .expect("load session"),
        Some(session.clone())
    );

    for attempt in 1..=5 {
        let throttled = repository
            .record_login_failure(source, 5, 60)
            .await
            .expect("record failure");
        assert_eq!(throttled, attempt == 5);
    }
    assert!(
        repository
            .login_source_is_throttled(source, 5, 60)
            .await
            .expect("read failure window")
    );

    let keys = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async::<Vec<String>>(&mut connection)
        .await
        .expect("list isolated test keys");
    assert_eq!(keys.len(), 2);
    for key in &keys {
        assert!(!key.contains(session_id));
        assert!(!key.contains(source));
        let ttl = redis::cmd("PTTL")
            .arg(key)
            .query_async::<i64>(&mut connection)
            .await
            .expect("read key ttl");
        assert!((1..=60_000).contains(&ttl));
        let value = redis::cmd("GET")
            .arg(key)
            .query_async::<String>(&mut connection)
            .await
            .expect("read isolated test value");
        assert!(!value.contains(session_id));
        assert!(!value.contains(source));
        assert!(!value.contains(password));
        assert!(!value.contains(admin_api_key));
    }

    repository
        .clear_login_failures(source)
        .await
        .expect("clear failure window");
    assert!(
        !repository
            .login_source_is_throttled(source, 5, 60)
            .await
            .expect("read cleared failure window")
    );
    assert_eq!(
        repository
            .delete_admin_session(session_id)
            .await
            .expect("delete session"),
        Some(session)
    );
    assert_eq!(
        repository
            .load_admin_session(session_id)
            .await
            .expect("load deleted session"),
        None
    );
}

async fn admin_auth_repository()
-> Option<(RedisAdminAuthStateRepository, ConnectionManager, String)> {
    let redis_url = std::env::var("CPR_TEST_REDIS_URL").ok()?;
    let client = redis::Client::open(redis_url).expect("valid CPR_TEST_REDIS_URL");
    let connection = client
        .get_connection_manager()
        .await
        .expect("connect test Redis");
    let namespace = format!("gateway-store-test-{}", Uuid::new_v4());
    let repository = RedisAdminAuthStateRepository::new(connection.clone(), &namespace)
        .expect("valid test namespace");
    Some((repository, connection, namespace))
}
