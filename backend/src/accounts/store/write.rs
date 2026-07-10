use super::*;

impl PgAccountStore {
    /// 构造存储。
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// 返回底层连接池。
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

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

    /// 读取单个账号。
    pub async fn get(&self, account_id: &str) -> PgAccountStoreResult<Option<StoredAccount>> {
        let row = sqlx::query(SELECT_STORED_ACCOUNT_BY_ID_SQL)
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(stored_account_from_row).transpose()
    }

    /// 通过 ChatGPT 身份查找账号。
    pub async fn find_by_chatgpt_identity(
        &self,
        chatgpt_account_id: &str,
        chatgpt_user_id: Option<&str>,
    ) -> PgAccountStoreResult<Option<StoredAccount>> {
        let row = sqlx::query(SELECT_STORED_ACCOUNT_BY_CHATGPT_IDENTITY_SQL)
            .bind(chatgpt_account_id)
            .bind(chatgpt_user_id)
            .bind(chatgpt_user_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(stored_account_from_row).transpose()
    }

    /// 分页列出所有账号（含 token）。
    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> PgAccountStoreResult<Page<StoredAccount>> {
        let limit = limit.clamp(1, 200);
        if let Some(cursor) = cursor {
            let (added_at, id) =
                decode_cursor(&cursor).ok_or(PgAccountStoreError::InvalidCursor)?;
            let added_at =
                parse_rfc3339_utc(&added_at).map_err(|_| PgAccountStoreError::InvalidCursor)?;
            let rows = sqlx::query(LIST_STORED_ACCOUNTS_AFTER_CURSOR_SQL)
                .bind(added_at)
                .bind(added_at)
                .bind(&id)
                .bind(i64::from(limit) + 1)
                .fetch_all(&self.pool)
                .await?;
            Ok(to_page(
                &rows,
                limit,
                stored_account_from_row,
                ("added_at", "id"),
            ))
        } else {
            let rows = sqlx::query(LIST_STORED_ACCOUNTS_SQL)
                .bind(i64::from(limit) + 1)
                .fetch_all(&self.pool)
                .await?;
            Ok(to_page(
                &rows,
                limit,
                stored_account_from_row,
                ("added_at", "id"),
            ))
        }
    }

    /// 分页列出账号元数据（不含 token）。
    pub async fn list_metadata(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> PgAccountStoreResult<Page<StoredAccountMetadata>> {
        let limit = limit.clamp(1, 200);
        if let Some(cursor) = cursor {
            let (added_at, id) =
                decode_cursor(&cursor).ok_or(PgAccountStoreError::InvalidCursor)?;
            let added_at =
                parse_rfc3339_utc(&added_at).map_err(|_| PgAccountStoreError::InvalidCursor)?;
            let rows = sqlx::query(LIST_ACCOUNT_METADATA_AFTER_CURSOR_SQL)
                .bind(added_at)
                .bind(added_at)
                .bind(&id)
                .bind(i64::from(limit) + 1)
                .fetch_all(&self.pool)
                .await?;
            Ok(to_page(&rows, limit, metadata_from_row, ("added_at", "id")))
        } else {
            let rows = sqlx::query(LIST_ACCOUNT_METADATA_SQL)
                .bind(i64::from(limit) + 1)
                .fetch_all(&self.pool)
                .await?;
            Ok(to_page(&rows, limit, metadata_from_row, ("added_at", "id")))
        }
    }

    /// 按页码列出账号元数据（不含 token）。
    pub async fn list_metadata_page(
        &self,
        page: u32,
        page_size: u32,
        search: Option<&str>,
    ) -> PgAccountStoreResult<NumberedPage<StoredAccountMetadata>> {
        let page_size = page_size.clamp(1, 200);
        let search = search.map(str::trim).filter(|value| !value.is_empty());
        let total = count_account_metadata(&self.pool, search).await?;
        let offset = page_offset(page, page_size);

        let mut builder = QueryBuilder::<Postgres>::new(LIST_ACCOUNT_METADATA_SELECT_SQL);
        push_account_metadata_search(&mut builder, search);
        builder.push(" order by added_at desc, id desc limit ");
        builder.push_bind(i64::from(page_size));
        builder.push(" offset ");
        builder.push_bind(offset.min(i64::MAX as u64) as i64);

        let rows = builder.build().fetch_all(&self.pool).await?;
        let items = rows
            .iter()
            .map(metadata_from_row)
            .collect::<PgAccountStoreResult<Vec<_>>>()?;

        Ok(NumberedPage {
            items,
            total,
            page: page.max(1),
            page_size,
        })
    }

    /// 读取单个账号元数据（不含 token）。
    pub async fn get_metadata(
        &self,
        account_id: &str,
    ) -> PgAccountStoreResult<Option<StoredAccountMetadata>> {
        let row = sqlx::query(SELECT_ACCOUNT_METADATA_BY_ID_SQL)
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(metadata_from_row).transpose()
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
        sync_usage_window: bool,
    ) -> PgAccountStoreResult<bool> {
        let now = Utc::now();
        let quota_limit_reached = account.quota_limit_reached;
        let quota_verify_required = account.quota_verify_required;
        let quota_cooldown_until = account.quota_cooldown_until;
        let cloudflare_cooldown_until = account.cloudflare_cooldown_until;
        let mut tx = self.pool.begin().await?;
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
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() == 0 {
            tx.commit().await?;
            return Ok(false);
        }

        if sync_usage_window {
            sqlx::query(SYNC_RUNTIME_ACCOUNT_USAGE_WINDOW_SQL)
                .bind(&account.id)
                .bind(u64_to_i64_saturating(account.window_request_count))
                .bind(u64_to_i64_saturating(account.window_input_tokens))
                .bind(u64_to_i64_saturating(account.window_output_tokens))
                .bind(u64_to_i64_saturating(account.window_cached_tokens))
                .bind(u64_to_i64_saturating(account.window_image_input_tokens))
                .bind(u64_to_i64_saturating(account.window_image_output_tokens))
                .bind(u64_to_i64_saturating(account.window_image_request_count))
                .bind(u64_to_i64_saturating(
                    account.window_image_request_failed_count,
                ))
                .bind(account.window_started_at)
                .bind(account.window_reset_at)
                .bind(account.limit_window_seconds.map(u64_to_i64_saturating))
                .bind(now)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        Ok(true)
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

    /// 记录用量。
    pub async fn record_usage(
        &self,
        account_id: &str,
        delta: UsageDelta,
    ) -> PgAccountStoreResult<()> {
        let last_used_at = Utc::now();
        sqlx::query(RECORD_USAGE_SQL)
            .bind(account_id)
            .bind(delta.request_count)
            .bind(delta.empty_response_count)
            .bind(delta.input_tokens)
            .bind(delta.output_tokens)
            .bind(delta.cached_tokens)
            .bind(delta.reasoning_tokens)
            .bind(delta.total_tokens)
            .bind(delta.image_input_tokens)
            .bind(delta.image_output_tokens)
            .bind(delta.image_request_count)
            .bind(delta.image_request_failed_count)
            .bind(delta.window_request_count)
            .bind(delta.window_input_tokens)
            .bind(delta.window_output_tokens)
            .bind(delta.window_cached_tokens)
            .bind(delta.window_image_input_tokens)
            .bind(delta.window_image_output_tokens)
            .bind(delta.window_image_request_count)
            .bind(delta.window_image_request_failed_count)
            .bind(last_used_at)
            .bind(last_used_at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 记录模型维度用量。
    pub async fn record_model_usage(
        &self,
        account_id: &str,
        model: &str,
        delta: AccountModelUsageDelta,
    ) -> PgAccountStoreResult<()> {
        let model = model.trim();
        if model.is_empty() {
            return Ok(());
        }
        let last_used_at = Utc::now();
        sqlx::query(RECORD_MODEL_USAGE_SQL)
            .bind(account_id)
            .bind(model)
            .bind(u64_to_i64_saturating(delta.requests))
            .bind(u64_to_i64_saturating(delta.errors))
            .bind(u64_to_i64_saturating(delta.input_tokens))
            .bind(u64_to_i64_saturating(delta.output_tokens))
            .bind(u64_to_i64_saturating(delta.cached_tokens))
            .bind(last_used_at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 读取配额快照列表。
    pub async fn list_quota_snapshots(&self) -> PgAccountStoreResult<Vec<AccountQuotaSnapshot>> {
        let rows = sqlx::query(LIST_QUOTA_SNAPSHOTS_SQL)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(quota_snapshot_from_row).collect()
    }

    /// 读取单账号配额 JSON。
    pub async fn get_quota_json(&self, account_id: &str) -> PgAccountStoreResult<Option<String>> {
        let row = sqlx::query("select quota_json from accounts where id = $1")
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row
            .and_then(|row| row.get::<Option<sqlx::types::Json<Value>>, _>("quota_json"))
            .map(|value| value.0.to_string()))
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

    /// 同步 rate-limit 窗口（含重置）。
    pub async fn sync_rate_limit_window(
        &self,
        account_id: &str,
        reset_at: DateTime<Utc>,
        limit_window_seconds: Option<u64>,
    ) -> PgAccountStoreResult<()> {
        let existing = sqlx::query(SELECT_RATE_LIMIT_WINDOW_SQL)
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?;

        let should_reset = existing
            .as_ref()
            .map(|row| {
                let existing_reset_at = row.get::<Option<DateTime<Utc>>, _>("window_reset_at");
                let existing_limit_window_seconds =
                    optional_positive_i64_to_u64(row.get::<Option<i64>, _>("limit_window_seconds"));
                should_reset_usage_window(
                    existing_reset_at,
                    existing_limit_window_seconds,
                    reset_at,
                    limit_window_seconds,
                )
            })
            .unwrap_or_default();

        if should_reset {
            let window_started_at = Utc::now();
            sqlx::query(SYNC_RATE_LIMIT_WINDOW_RESET_SQL)
                .bind(account_id)
                .bind(window_started_at)
                .bind(reset_at)
                .bind(limit_window_seconds.map(u64_to_i64_saturating))
                .execute(&self.pool)
                .await?;
        } else {
            sqlx::query(SYNC_RATE_LIMIT_WINDOW_SQL)
                .bind(account_id)
                .bind(reset_at)
                .bind(limit_window_seconds.map(u64_to_i64_saturating))
                .execute(&self.pool)
                .await?;
        }
        Ok(())
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
