use chrono::Duration;
use codex_proxy_rs::{auth::store::RedisAdminSessionStore, infra::redis::RedisConnection};
use sqlx::PgPool;

pub(crate) async fn seed_admin_session(pool: &PgPool, redis: &RedisConnection, session_id: &str) {
    sqlx::query(
        "insert into admin_users (id, password_hash, created_at, updated_at)
         values ($1, $2, now(), now()) on conflict (id) do nothing",
    )
    .bind("admin_1")
    .bind("hash")
    .execute(pool)
    .await
    .unwrap();

    RedisAdminSessionStore::new(redis.clone())
        .create_session(session_id, "admin_1", Duration::days(1))
        .await
        .unwrap();
}
