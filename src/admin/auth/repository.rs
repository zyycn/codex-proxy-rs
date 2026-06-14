use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminUserRecord {
    pub id: String,
    pub password_hash: String,
}

#[derive(Clone)]
pub struct AdminAuthRepository {
    pool: SqlitePool,
}

impl AdminAuthRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn ensure_default_admin(&self, password_hash: &str) -> Result<bool, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let count: (i64,) = sqlx::query_as("select count(*) from admin_users")
            .fetch_one(&mut *tx)
            .await?;
        if count.0 > 0 {
            tx.commit().await?;
            return Ok(false);
        }

        let admin_id = format!("admin_{}", Uuid::new_v4().simple());
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "insert into admin_users (id, password_hash, created_at, updated_at) values (?, ?, ?, ?)",
        )
        .bind(admin_id)
        .bind(password_hash)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn load_first_admin(&self) -> Result<Option<AdminUserRecord>, sqlx::Error> {
        let row = sqlx::query(
            "select id, password_hash from admin_users order by created_at asc limit 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| AdminUserRecord {
            id: row.get("id"),
            password_hash: row.get("password_hash"),
        }))
    }

    pub async fn create_session(
        &self,
        session_id: &str,
        user_id: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "insert into admin_sessions (id, user_id, expires_at, created_at) values (?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind(user_id)
        .bind(expires_at.to_rfc3339())
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn validate_session(&self, session_id: &str) -> Result<bool, sqlx::Error> {
        let now = Utc::now().to_rfc3339();
        let row = sqlx::query("select 1 from admin_sessions where id = ? and expires_at > ?")
            .bind(session_id)
            .bind(now)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    pub async fn cleanup_expired_sessions(&self, now: DateTime<Utc>) -> Result<u64, sqlx::Error> {
        let result = sqlx::query("delete from admin_sessions where expires_at < ?")
            .bind(now.to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}
