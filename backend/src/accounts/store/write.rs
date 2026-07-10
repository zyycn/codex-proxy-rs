use super::*;

impl PgAccountStore {
    /// 插入新账号。
    pub async fn insert(&self, account: NewAccount) -> PgAccountStoreResult<()> {
        let now = Utc::now();
        let refresh_token = account
            .refresh_token
            .as_ref()
            .map(ExposeSecret::expose_secret);
        sqlx::query(INSERT_ACCOUNT_SQL)
            .bind(&account.id)
            .bind(&account.email)
            .bind(&account.account_id)
            .bind(&account.user_id)
            .bind(&account.label)
            .bind(&account.plan_type)
            .bind(account.access_token.expose_secret())
            .bind(refresh_token)
            .bind(account.access_token_expires_at)
            .bind(status_to_db(account.status))
            .bind(account.added_at.unwrap_or(now))
            .bind(now)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 更新单账号元数据（不含 token）。
    pub async fn update_metadata(
        &self,
        account_id: &str,
        update: AccountMetadataUpdate,
    ) -> PgAccountStoreResult<bool> {
        if !update.any() {
            return Ok(false);
        }
        let now = Utc::now();
        let status = update.status.map(status_to_db);
        let result = sqlx::query(UPDATE_ACCOUNT_METADATA_SQL)
            .bind(update.email.is_some())
            .bind(optional_update_value(&update.email))
            .bind(update.account_id.is_some())
            .bind(optional_update_value(&update.account_id))
            .bind(update.user_id.is_some())
            .bind(optional_update_value(&update.user_id))
            .bind(update.label.is_some())
            .bind(optional_update_value(&update.label))
            .bind(update.plan_type.is_some())
            .bind(optional_update_value(&update.plan_type))
            .bind(update.status.is_some())
            .bind(status)
            .bind(status)
            .bind(now)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 更新账号 claims（含 refresh token）。
    pub async fn update_from_claims(
        &self,
        account_id: &str,
        update: AccountClaimsUpdate,
    ) -> PgAccountStoreResult<bool> {
        let now = Utc::now();
        let refresh_token = update
            .refresh_token
            .as_ref()
            .map(ExposeSecret::expose_secret);

        let result = if let Some(refresh_token) = refresh_token {
            sqlx::query(UPDATE_ACCOUNT_CLAIMS_WITH_REFRESH_SQL)
                .bind(&update.email)
                .bind(&update.account_id)
                .bind(&update.user_id)
                .bind(&update.plan_type)
                .bind(update.access_token.expose_secret())
                .bind(refresh_token)
                .bind(update.access_token_expires_at)
                .bind(update.next_refresh_at)
                .bind(status_to_db(update.status))
                .bind(now)
                .bind(account_id)
                .execute(&self.pool)
                .await?
        } else {
            sqlx::query(UPDATE_ACCOUNT_CLAIMS_PRESERVING_REFRESH_SQL)
                .bind(&update.email)
                .bind(&update.account_id)
                .bind(&update.user_id)
                .bind(&update.plan_type)
                .bind(update.access_token.expose_secret())
                .bind(update.access_token_expires_at)
                .bind(update.next_refresh_at)
                .bind(status_to_db(update.status))
                .bind(now)
                .bind(account_id)
                .execute(&self.pool)
                .await?
        };
        Ok(result.rows_affected() > 0)
    }

    /// 通过导入数据更新已有账号。
    pub async fn update_from_import(
        &self,
        update: ImportedAccountUpdate,
    ) -> PgAccountStoreResult<bool> {
        let now = Utc::now();
        let refresh_token = update
            .account
            .refresh_token
            .as_ref()
            .map(ExposeSecret::expose_secret);
        let quota_json = update
            .quota_json
            .map(|value| serde_json::from_str::<Value>(&value))
            .transpose()?
            .map(sqlx::types::Json);
        let quota_fetched_at = update.quota_fetched_at;

        let result = if let Some(refresh_token) = refresh_token {
            sqlx::query(UPDATE_IMPORTED_ACCOUNT_WITH_REFRESH_SQL)
                .bind(&update.account.email)
                .bind(&update.account.account_id)
                .bind(&update.account.user_id)
                .bind(&update.account.label)
                .bind(&update.account.plan_type)
                .bind(update.account.access_token.expose_secret())
                .bind(refresh_token)
                .bind(update.account.access_token_expires_at)
                .bind(status_to_db(update.account.status))
                .bind(&quota_json)
                .bind(quota_fetched_at)
                .bind(quota_fetched_at)
                .bind(update.quota_verify_required)
                .bind(now)
                .bind(&update.account.id)
                .execute(&self.pool)
                .await?
        } else {
            sqlx::query(UPDATE_IMPORTED_ACCOUNT_PRESERVING_REFRESH_SQL)
                .bind(&update.account.email)
                .bind(&update.account.account_id)
                .bind(&update.account.user_id)
                .bind(&update.account.label)
                .bind(&update.account.plan_type)
                .bind(update.account.access_token.expose_secret())
                .bind(update.account.access_token_expires_at)
                .bind(status_to_db(update.account.status))
                .bind(&quota_json)
                .bind(quota_fetched_at)
                .bind(quota_fetched_at)
                .bind(update.quota_verify_required)
                .bind(now)
                .bind(&update.account.id)
                .execute(&self.pool)
                .await?
        };
        Ok(result.rows_affected() > 0)
    }

    /// 设置下一次刷新时间。
    pub async fn set_next_refresh_at(
        &self,
        account_id: &str,
        next_refresh_at: Option<DateTime<Utc>>,
    ) -> PgAccountStoreResult<bool> {
        let now = Utc::now();
        let result = sqlx::query(SET_NEXT_REFRESH_AT_SQL)
            .bind(next_refresh_at)
            .bind(now)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 标记账号进入配额冷却期。
    pub async fn mark_quota_limited_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> PgAccountStoreResult<bool> {
        let now = Utc::now();
        let result = sqlx::query(MARK_QUOTA_LIMITED_UNTIL_SQL)
            .bind(cooldown_until)
            .bind(now)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 同步运行时自然刷新出来的账号状态。
    pub async fn sync_runtime_account_state(
        &self,
        account: &Account,
    ) -> PgAccountStoreResult<bool> {
        let now = Utc::now();
        let quota_limit_reached = account.quota_limit_reached;
        let quota_verify_required = account.quota_verify_required;
        let quota_cooldown_until = account.quota_cooldown_until;
        let cloudflare_cooldown_until = account.cloudflare_cooldown_until;
        let result = sqlx::query(SYNC_RUNTIME_ACCOUNT_STATE_SQL)
            .bind(quota_limit_reached)
            .bind(now)
            .bind(quota_limit_reached)
            .bind(status_to_db(account.status))
            .bind(quota_limit_reached)
            .bind(now)
            .bind(quota_limit_reached)
            .bind(quota_verify_required)
            .bind(now)
            .bind(quota_verify_required)
            .bind(quota_cooldown_until)
            .bind(now)
            .bind(quota_cooldown_until)
            .bind(cloudflare_cooldown_until)
            .bind(now)
            .bind(cloudflare_cooldown_until)
            .bind(now)
            .bind(&account.id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 标记账号 Cloudflare 冷却期。
    pub async fn set_cloudflare_cooldown_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> PgAccountStoreResult<bool> {
        let now = Utc::now();
        let result = sqlx::query(SET_CLOUDFLARE_COOLDOWN_UNTIL_SQL)
            .bind(cooldown_until)
            .bind(now)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 更新账号状态。
    pub async fn set_status(
        &self,
        account_id: &str,
        status: AccountStatus,
    ) -> PgAccountStoreResult<bool> {
        let now = Utc::now();
        let status = status_to_db(status);
        let result = sqlx::query(
            r"
update accounts
set
  status = case
    when $1 = 'active' and quota_limit_reached then 'quota_exhausted'
    else $2
  end,
  updated_at = $3
where id = $4",
        )
        .bind(status)
        .bind(status)
        .bind(now)
        .bind(account_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 更新配额 JSON。
    pub async fn update_quota_json(
        &self,
        account_id: &str,
        quota_json: &str,
    ) -> PgAccountStoreResult<bool> {
        let now = Utc::now();
        let plan_type = quota_plan_type(quota_json);
        let quota_json = sqlx::types::Json(serde_json::from_str::<Value>(quota_json)?);
        let result = sqlx::query(UPDATE_QUOTA_JSON_SQL)
            .bind(quota_json)
            .bind(now)
            .bind(&plan_type)
            .bind(now)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 应用配额快照。
    pub async fn apply_quota_snapshot(
        &self,
        account_id: &str,
        quota_json: &str,
        limit_reached: bool,
        cooldown_until: Option<DateTime<Utc>>,
    ) -> PgAccountStoreResult<bool> {
        let now = Utc::now();
        let cooldown = cooldown_until;
        let plan_type = quota_plan_type(quota_json);
        let quota_json = sqlx::types::Json(serde_json::from_str::<Value>(quota_json)?);
        let result = sqlx::query(APPLY_QUOTA_SNAPSHOT_SQL)
            .bind(quota_json)
            .bind(now)
            .bind(&plan_type)
            .bind(limit_reached)
            .bind(limit_reached)
            .bind(cooldown)
            .bind(now)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 应用导入的配额状态。
    pub async fn apply_imported_quota_state(
        &self,
        account_id: &str,
        quota_json: Option<&str>,
        quota_fetched_at: Option<DateTime<Utc>>,
        quota_verify_required: bool,
    ) -> PgAccountStoreResult<bool> {
        let now = Utc::now();
        let fetched = quota_fetched_at;
        let plan_type = quota_json.and_then(quota_plan_type);
        let quota_json = quota_json
            .map(serde_json::from_str::<Value>)
            .transpose()?
            .map(sqlx::types::Json);
        let result = sqlx::query(APPLY_IMPORTED_QUOTA_STATE_SQL)
            .bind(quota_json)
            .bind(fetched)
            .bind(fetched)
            .bind(&plan_type)
            .bind(quota_verify_required)
            .bind(now)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 删除单个账号。
    pub async fn delete(&self, account_id: &str) -> PgAccountStoreResult<bool> {
        let result = sqlx::query(DELETE_ACCOUNT_SQL)
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}
