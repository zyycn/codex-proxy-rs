use std::time::Duration;

use chrono::Utc;
use gateway_store::redis::{
    AuthSessionRecord, AuthStateRepository, RedisAuthStateRepository, SessionSubjectRecord,
};
use redis::aio::ConnectionManager;
use uuid::Uuid;

#[tokio::test]
async fn client_session_should_use_fixed_ttl_without_storing_session_or_api_key_secrets() {
    let Some((repository, mut connection, namespace)) = auth_repository().await else {
        return;
    };
    let session_id = "client_session_real_secret";
    let raw_api_key = "cpr_raw_key_must_never_enter_redis";
    let session = AuthSessionRecord {
        subject: SessionSubjectRecord::Key {
            client_key_id: "key-42".to_owned(),
        },
        expires_at: Utc::now() + chrono::Duration::seconds(60),
    };

    repository
        .store_session(session_id, &session)
        .await
        .expect("store client session");
    assert_eq!(
        repository
            .load_session(session_id)
            .await
            .expect("load client session"),
        Some(session.clone())
    );

    let keys = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async::<Vec<String>>(&mut connection)
        .await
        .expect("list isolated client state keys");
    assert_eq!(keys.len(), 1);
    let key = &keys[0];
    assert!(!key.contains(session_id));
    assert!(!key.contains(raw_api_key));
    let ttl = redis::cmd("PTTL")
        .arg(key)
        .query_async::<i64>(&mut connection)
        .await
        .expect("read client session ttl");
    assert!((1..=60_000).contains(&ttl));
    let payload = redis::cmd("GET")
        .arg(key)
        .query_async::<String>(&mut connection)
        .await
        .expect("read client session payload");
    assert!(payload.contains("key-42"));
    assert!(!payload.contains(session_id));
    assert!(!payload.contains(raw_api_key));

    assert_eq!(
        repository
            .delete_session(session_id)
            .await
            .expect("delete client session"),
        Some(session)
    );
}

#[tokio::test]
async fn login_rate_limit_should_enforce_source_and_global_buckets_atomically() {
    let Some((repository, _connection, _namespace)) = auth_repository().await else {
        return;
    };
    let window = Duration::from_secs(60);

    assert_eq!(
        repository
            .consume_login_attempt("203.0.113.10", 2, 3, window)
            .await
            .expect("first source attempt"),
        None
    );
    assert_eq!(
        repository
            .consume_login_attempt("203.0.113.10", 2, 3, window)
            .await
            .expect("second source attempt"),
        None
    );
    assert!(
        repository
            .consume_login_attempt("203.0.113.10", 2, 3, window)
            .await
            .expect("source rejection")
            .is_some()
    );
    assert!(
        repository
            .consume_login_attempt("203.0.113.11", 2, 3, window)
            .await
            .expect("global rejection")
            .is_some()
    );
}

#[tokio::test]
async fn client_session_should_reject_invalid_intervals_and_rate_limit_policies() {
    let Some((repository, _connection, _namespace)) = auth_repository().await else {
        return;
    };
    let now = Utc::now();
    let invalid = AuthSessionRecord {
        subject: SessionSubjectRecord::Key {
            client_key_id: "key-42".to_owned(),
        },
        expires_at: now,
    };

    assert!(repository.store_session("invalid", &invalid).await.is_err());
    assert!(
        repository
            .consume_login_attempt("203.0.113.10", 0, 1, Duration::from_secs(60))
            .await
            .is_err()
    );
    assert!(
        repository
            .consume_login_attempt("203.0.113.10", 1, 1, Duration::ZERO)
            .await
            .is_err()
    );
}

async fn auth_repository() -> Option<(RedisAuthStateRepository, ConnectionManager, String)> {
    let redis_url = crate::support::test_env("CPR_TEST_REDIS_URL")?;
    let client = redis::Client::open(redis_url).expect("valid CPR_TEST_REDIS_URL");
    let connection = client
        .get_connection_manager()
        .await
        .expect("connect test Redis");
    let namespace = format!("gateway-store-test-{}", Uuid::new_v4());
    let repository = RedisAuthStateRepository::new(connection.clone(), &namespace)
        .expect("valid client state namespace");
    let full_namespace = format!("{namespace}:auth:v1");
    Some((repository, connection, full_namespace))
}

#[test]
fn admin_auth_state_rejects_invalid_ttl_boundaries() {
    let session = AuthSessionRecord {
        subject: SessionSubjectRecord::Admin {
            credential_fingerprint: String::new(),
            admin_user_id: "admin".to_owned(),
        },
        expires_at: Utc::now() - chrono::Duration::seconds(1),
    };
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let Some((repository, _connection, _namespace)) = runtime.block_on(auth_repository()) else {
        return;
    };
    assert!(
        runtime
            .block_on(repository.store_session("expired-session", &session))
            .is_err()
    );
}

#[tokio::test]
async fn admin_auth_state_keeps_fixed_ttl_and_opaque_keys() {
    let Some((repository, mut connection, namespace)) = auth_repository().await else {
        return;
    };
    let session_id = "session-real-secret-id";
    let password = "must-never-enter-redis";
    let admin_api_key = "admin-must-never-enter-redis";
    let session = AuthSessionRecord {
        subject: SessionSubjectRecord::Admin {
            credential_fingerprint: "test-password-fingerprint".to_owned(),
            admin_user_id: "default-admin".to_owned(),
        },
        expires_at: Utc::now() + chrono::Duration::seconds(60),
    };

    repository
        .store_session(session_id, &session)
        .await
        .expect("store session");
    assert_eq!(
        repository
            .load_session(session_id)
            .await
            .expect("load session"),
        Some(session.clone())
    );

    let keys = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async::<Vec<String>>(&mut connection)
        .await
        .expect("list isolated test keys");
    assert_eq!(keys.len(), 1);
    for key in &keys {
        assert!(!key.contains(session_id));
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
        assert!(!value.contains(password));
        assert!(!value.contains(admin_api_key));
    }

    assert_eq!(
        repository
            .delete_session(session_id)
            .await
            .expect("delete session"),
        Some(session)
    );
    assert_eq!(
        repository
            .load_session(session_id)
            .await
            .expect("load deleted session"),
        None
    );
}

#[tokio::test]
async fn unified_session_rejects_invalid_identities_and_loads_legacy_admin_payloads() {
    let Some((repository, mut connection, namespace)) = auth_repository().await else {
        return;
    };
    let token = "malformed-session-fixture";
    repository
        .store_session(
            token,
            &AuthSessionRecord {
                subject: SessionSubjectRecord::Key {
                    client_key_id: "key-42".to_owned(),
                },
                expires_at: Utc::now() + chrono::Duration::seconds(60),
            },
        )
        .await
        .unwrap();
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("{namespace}:session:*"))
        .query_async(&mut connection)
        .await
        .unwrap();
    assert_eq!(keys.len(), 1);
    for subject in [
        serde_json::json!({"type": "superadmin", "admin_user_id": "admin"}),
        serde_json::json!({"type": "admin", "admin_user_id": "admin", "client_key_id": "key-42"}),
        serde_json::json!({"type": "key", "client_key_id": ""}),
        serde_json::json!({"type": "key", "admin_user_id": "admin"}),
    ] {
        let payload = serde_json::json!({"subject": subject, "expires_at": (Utc::now() + chrono::Duration::seconds(60)).to_rfc3339()});
        redis::cmd("SET")
            .arg(&keys[0])
            .arg(payload.to_string())
            .arg("EX")
            .arg(60)
            .query_async::<()>(&mut connection)
            .await
            .unwrap();
        assert!(repository.load_session(token).await.is_err());
    }

    // 旧版会话仍能解码，空指纹交由认证服务判定失效，避免升级后返回存储故障。
    let legacy = serde_json::json!({
        "subject": {"type": "admin", "admin_user_id": "admin"},
        "expires_at": (Utc::now() + chrono::Duration::seconds(60)).to_rfc3339(),
    });
    redis::cmd("SET")
        .arg(&keys[0])
        .arg(legacy.to_string())
        .arg("EX")
        .arg(60)
        .query_async::<()>(&mut connection)
        .await
        .unwrap();
    assert_eq!(
        repository
            .load_session(token)
            .await
            .unwrap()
            .unwrap()
            .subject,
        SessionSubjectRecord::Admin {
            admin_user_id: "admin".to_owned(),
            credential_fingerprint: String::new(),
        }
    );
}
