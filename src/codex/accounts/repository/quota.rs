use chrono::{DateTime, Utc};
use sqlx::Row;

use super::{
    parse_optional_rfc3339, AccountQuotaSnapshot, AccountRepository, AccountRepositoryResult,
};

const LIST_QUOTA_SNAPSHOTS_SQL: &str = r"
select
  id,
  email,
  quota_json,
  quota_fetched_at
from accounts
where quota_json is not null
  and trim(quota_json) <> ''
order by coalesce(quota_fetched_at, '') desc, id desc";

const UPDATE_QUOTA_JSON_SQL: &str = r"
update accounts
set
  quota_json = ?,
  quota_fetched_at = ?,
  updated_at = ?
where id = ?";

const SET_QUOTA_COOLDOWN_UNTIL_SQL: &str = r"
update accounts
set
  quota_limit_reached = 1,
  quota_cooldown_until = case
    when quota_cooldown_until is not null and quota_cooldown_until > ?
    then quota_cooldown_until
    else ?
  end,
  updated_at = ?
where id = ?";

const SET_CLOUDFLARE_COOLDOWN_UNTIL_SQL: &str = r"
update accounts
set
  cloudflare_cooldown_until = ?,
  updated_at = ?
where id = ?";

impl AccountRepository {
    pub async fn list_quota_snapshots(&self) -> AccountRepositoryResult<Vec<AccountQuotaSnapshot>> {
        let rows = sqlx::query(LIST_QUOTA_SNAPSHOTS_SQL)
            .fetch_all(&self.pool)
            .await?;
        let mut snapshots = Vec::with_capacity(rows.len());
        for row in rows {
            snapshots.push(quota_snapshot_from_row(&row)?);
        }
        Ok(snapshots)
    }

    pub async fn update_quota_json(
        &self,
        account_id: &str,
        quota_json: &str,
    ) -> AccountRepositoryResult<bool> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(UPDATE_QUOTA_JSON_SQL)
            .bind(quota_json)
            .bind(&now)
            .bind(now)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn get_quota_json(
        &self,
        account_id: &str,
    ) -> AccountRepositoryResult<Option<String>> {
        let row = sqlx::query("select quota_json from accounts where id = ?")
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|row| row.get("quota_json")))
    }

    pub async fn set_quota_cooldown_until(
        &self,
        id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> AccountRepositoryResult<bool> {
        let cooldown_until = cooldown_until.to_rfc3339();
        let result = sqlx::query(SET_QUOTA_COOLDOWN_UNTIL_SQL)
            .bind(&cooldown_until)
            .bind(cooldown_until)
            .bind(Utc::now().to_rfc3339())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn set_cloudflare_cooldown_until(
        &self,
        id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> AccountRepositoryResult<bool> {
        let result = sqlx::query(SET_CLOUDFLARE_COOLDOWN_UNTIL_SQL)
            .bind(cooldown_until.to_rfc3339())
            .bind(Utc::now().to_rfc3339())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}

fn quota_snapshot_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> AccountRepositoryResult<AccountQuotaSnapshot> {
    Ok(AccountQuotaSnapshot {
        account_id: row.get("id"),
        email: row.get("email"),
        quota_json: row.get("quota_json"),
        quota_fetched_at: parse_optional_rfc3339(row.get::<Option<String>, _>("quota_fetched_at"))?,
    })
}
