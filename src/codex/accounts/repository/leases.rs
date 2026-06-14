use chrono::{DateTime, Utc};

use super::{AccountRepository, AccountRepositoryResult};

const TRY_ACQUIRE_REFRESH_LEASE_SQL: &str = r"
insert into account_refresh_leases (
  account_id,
  owner,
  expires_at,
  updated_at
) values (?, ?, ?, ?)
on conflict(account_id) do update set
  owner = excluded.owner,
  expires_at = excluded.expires_at,
  updated_at = excluded.updated_at
where account_refresh_leases.expires_at <= ?
  or account_refresh_leases.owner = excluded.owner";

impl AccountRepository {
    pub async fn try_acquire_refresh_lease(
        &self,
        account_id: &str,
        owner: &str,
        lease_until: DateTime<Utc>,
    ) -> AccountRepositoryResult<bool> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(TRY_ACQUIRE_REFRESH_LEASE_SQL)
            .bind(account_id)
            .bind(owner)
            .bind(lease_until.to_rfc3339())
            .bind(&now)
            .bind(now)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn release_refresh_lease(
        &self,
        account_id: &str,
        owner: &str,
    ) -> AccountRepositoryResult<bool> {
        let result =
            sqlx::query("delete from account_refresh_leases where account_id = ? and owner = ?")
                .bind(account_id)
                .bind(owner)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected() > 0)
    }
}
