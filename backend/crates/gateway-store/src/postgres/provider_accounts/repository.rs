//! Pg 账号 repository：Core/Admin 端口实现与 admin 事务。

use super::*;

#[async_trait]
pub trait ProviderAccountRepository: Send + Sync {
    async fn load_provider_account(&self, id: &str) -> StoreResult<Option<ProviderAccountRecord>>;
    async fn list_provider_accounts(
        &self,
        provider_kind: Option<&str>,
        include_disabled: bool,
    ) -> StoreResult<Vec<ProviderAccountSummary>>;
    async fn insert_provider_account(&self, account: NewProviderAccount) -> StoreResult<()>;
    async fn update_provider_account(&self, account: UpdateProviderAccount) -> StoreResult<bool>;
    async fn compare_and_swap_credentials(
        &self,
        update: ProviderCredentialUpdate,
    ) -> StoreResult<Revision>;
    async fn apply_provider_account_state(
        &self,
        update: ProviderAccountStateUpdate,
    ) -> StoreResult<bool>;
    async fn set_provider_account_enabled(&self, id: &str, enabled: bool) -> StoreResult<bool>;
    async fn compare_and_swap_provider_quota(
        &self,
        account_id: &str,
        expected_revision: Revision,
        quota: Option<JsonObject>,
        observed_at: Option<DateTime<Utc>>,
        limit_reached: Option<bool>,
    ) -> StoreResult<bool>;
    async fn delete_provider_account(&self, id: &str) -> StoreResult<bool>;
}

#[async_trait]
pub trait ProviderAccountAdminRepository: Send + Sync {
    async fn export_provider_accounts(
        &self,
        scope: ProviderAccountAdminScope,
        account_ids: Vec<String>,
    ) -> StoreResult<Vec<ProviderAccountRecord>>;

    async fn import_provider_accounts(
        &self,
        command: ImportProviderAccounts,
    ) -> StoreResult<ProviderAccountAdminImport>;

    async fn rotate_provider_account(
        &self,
        command: RotateProviderAccount,
    ) -> StoreResult<ProviderAccountAdminRotation>;

    async fn set_provider_account_enabled_admin(
        &self,
        command: SetProviderAccountEnabled,
    ) -> StoreResult<Revision>;

    async fn delete_provider_accounts_admin(
        &self,
        command: DeleteProviderAccounts,
    ) -> StoreResult<Revision>;
}

#[derive(Clone)]
pub struct PgProviderAccountRepository {
    pub(crate) pool: PgPool,
}

impl PgProviderAccountRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ProviderAccountRepository for PgProviderAccountRepository {
    async fn load_provider_account(&self, id: &str) -> StoreResult<Option<ProviderAccountRecord>> {
        require_nonempty(ENTITY, "id", id)?;
        let row = sqlx::query(ACCOUNT_SELECT)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| postgres_unavailable("load provider account"))?;
        row.map(account_record_from_row).transpose()
    }

    async fn list_provider_accounts(
        &self,
        provider_kind: Option<&str>,
        include_disabled: bool,
    ) -> StoreResult<Vec<ProviderAccountSummary>> {
        let rows = sqlx::query(
            "select id, provider_kind, name, email, upstream_user_id,
                    upstream_account_id, plan_type, authentication_kind, credential_revision, has_refresh_token,
                    access_token_expires_at, next_refresh_at, enabled, availability,
                    availability_observed_at,
                    quota_observed_at, quota_limit_reached, last_error_message, created_at, updated_at
             from provider_accounts
             where ($1::text is null or provider_kind = $1) and ($2 or enabled)
             order by provider_kind, name, id",
        )
        .bind(provider_kind)
        .bind(include_disabled)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("list provider accounts"))?;
        rows.into_iter().map(account_summary_from_row).collect()
    }

    async fn insert_provider_account(&self, account: NewProviderAccount) -> StoreResult<()> {
        account.validate()?;
        sqlx::query(
            "insert into provider_accounts (
               id, provider_kind, name, email, upstream_user_id,
               upstream_account_id, plan_type, authentication_kind, provider_credentials_json, credential_revision,
               has_refresh_token, access_token_expires_at, next_refresh_at, enabled,
               availability, provider_quota_json,
               availability_observed_at, quota_observed_at, created_at, updated_at
             ) values (
               $1, $2, $3, $4, $5, $6, $7, $8, $9, 1, $10, $11, $12, $13,
               $14, null, $15, null, now(), greatest(now(), $15)
             )",
        )
        .bind(account.id)
        .bind(account.provider_kind)
        .bind(account.name)
        .bind(account.email)
        .bind(account.upstream_user_id)
        .bind(account.upstream_account_id)
        .bind(account.plan_type)
        .bind(account.authentication_kind)
        .bind(account.provider_credentials_json.as_value())
        .bind(account.has_refresh_token)
        .bind(account.access_token_expires_at)
        .bind(account.next_refresh_at)
        .bind(account.enabled)
        .bind(account.availability.as_str())
        .bind(account.availability_observed_at)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("insert provider account"))?;
        Ok(())
    }

    async fn update_provider_account(&self, account: UpdateProviderAccount) -> StoreResult<bool> {
        require_nonempty(ENTITY, "id", &account.id)?;
        require_nonempty(ENTITY, "name", &account.name)?;
        let result = sqlx::query(
            "update provider_accounts
             set name = $2, email = $3, plan_type = $4, updated_at = now()
             where id = $1",
        )
        .bind(account.id)
        .bind(account.name)
        .bind(account.email)
        .bind(account.plan_type)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("update provider account"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn compare_and_swap_credentials(
        &self,
        update: ProviderCredentialUpdate,
    ) -> StoreResult<Revision> {
        require_nonempty(ENTITY, "account_id", &update.account_id)?;
        validate_object_size(
            "provider_credentials_json",
            &update.provider_credentials_json,
            CREDENTIALS_MAX_BYTES,
        )?;
        if !update.has_refresh_token && update.next_refresh_at.is_some() {
            return Err(invalid("next_refresh_at requires a refresh token"));
        }
        let next = sqlx::query_scalar::<_, i64>(
            "update provider_accounts
             set provider_credentials_json = $3,
                 credential_revision = credential_revision + 1,
                 has_refresh_token = $4,
                 access_token_expires_at = $5,
                 next_refresh_at = $6,
                 updated_at = now()
             where id = $1 and credential_revision = $2
             returning credential_revision",
        )
        .bind(&update.account_id)
        .bind(to_i64(update.expected_revision.get())?)
        .bind(update.provider_credentials_json.as_value())
        .bind(update.has_refresh_token)
        .bind(update.access_token_expires_at)
        .bind(update.next_refresh_at)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("compare and swap provider credentials"))?
        .ok_or(StoreError::Conflict {
            entity: ENTITY,
            id: update.account_id,
            kind: ConflictKind::StaleRevision,
        })?;
        Revision::new(to_u64(next)?)
    }

    async fn apply_provider_account_state(
        &self,
        update: ProviderAccountStateUpdate,
    ) -> StoreResult<bool> {
        update.validate()?;
        let result = sqlx::query(
            "update provider_accounts
             set availability = case when enabled then $3 else availability end,
                 availability_observed_at = case
                     when enabled then $4
                     else availability_observed_at
                 end,
                 last_error_message = case
                     when enabled then
                         case when $3 = 'ready' then null else coalesce($5, last_error_message) end
                     else last_error_message
                 end,
                 updated_at = case when enabled then greatest(now(), $4) else updated_at end
             where id = $1 and credential_revision = $2
               and (availability_observed_at is null or availability_observed_at <= $4)",
        )
        .bind(update.account_id)
        .bind(to_i64(update.expected_revision.get())?)
        .bind(update.availability.as_str())
        .bind(update.availability_observed_at)
        .bind(update.message.as_deref())
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("apply provider account state"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn set_provider_account_enabled(&self, id: &str, enabled: bool) -> StoreResult<bool> {
        require_nonempty(ENTITY, "id", id)?;
        let result = sqlx::query(
            "update provider_accounts set enabled = $2, updated_at = now() where id = $1",
        )
        .bind(id)
        .bind(enabled)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("set provider account enabled state"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn compare_and_swap_provider_quota(
        &self,
        account_id: &str,
        expected_revision: Revision,
        quota: Option<JsonObject>,
        observed_at: Option<DateTime<Utc>>,
        limit_reached: Option<bool>,
    ) -> StoreResult<bool> {
        require_nonempty(ENTITY, "account_id", account_id)?;
        if quota.is_some() != observed_at.is_some() {
            return Err(invalid("quota JSON and observed_at must agree"));
        }
        if let Some(quota) = quota.as_ref() {
            validate_object_size("provider_quota_json", quota, QUOTA_MAX_BYTES)?;
        }
        let result = sqlx::query(
            "update provider_accounts
             set provider_quota_json = $3, quota_observed_at = $4,
                 quota_limit_reached = coalesce($5, quota_limit_reached),
                 updated_at = greatest(now(), coalesce($4, now()))
             where id = $1 and credential_revision = $2
               and ($4 is null or quota_observed_at is null or quota_observed_at <= $4)",
        )
        .bind(account_id)
        .bind(to_i64(expected_revision.get())?)
        .bind(quota.map(|value| value.as_value()))
        .bind(observed_at)
        .bind(limit_reached)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("compare and swap provider quota"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn delete_provider_account(&self, id: &str) -> StoreResult<bool> {
        require_nonempty(ENTITY, "id", id)?;
        let result = sqlx::query("delete from provider_accounts where id = $1 and not enabled")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|_| postgres_unavailable("delete disabled provider account"))?;
        Ok(result.rows_affected() == 1)
    }
}

#[async_trait]
impl ProviderAccountAdminRepository for PgProviderAccountRepository {
    async fn export_provider_accounts(
        &self,
        scope: ProviderAccountAdminScope,
        account_ids: Vec<String>,
    ) -> StoreResult<Vec<ProviderAccountRecord>> {
        scope.validate()?;
        validate_admin_account_ids(&account_ids)?;
        let rows = sqlx::query(ACCOUNT_SELECT_BY_IDS)
            .bind(&account_ids)
            .bind(&scope.provider_kind)
            .fetch_all(&self.pool)
            .await
            .map_err(|_| postgres_unavailable("export provider accounts"))?;
        let records = rows
            .into_iter()
            .map(account_record_from_row)
            .collect::<StoreResult<Vec<_>>>()?;
        if records.len() != account_ids.len() {
            return Err(invalid(
                "one or more exported accounts are missing or outside the Provider scope",
            ));
        }
        let by_id = records
            .into_iter()
            .map(|record| (record.summary.id.clone(), record))
            .collect::<std::collections::HashMap<_, _>>();
        account_ids
            .into_iter()
            .map(|id| {
                by_id.get(&id).cloned().ok_or_else(|| {
                    invalid("one or more exported accounts are missing after loading")
                })
            })
            .collect()
    }

    async fn import_provider_accounts(
        &self,
        command: ImportProviderAccounts,
    ) -> StoreResult<ProviderAccountAdminImport> {
        command.validate()?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin provider account admin import"))?;
        let result = async {
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            let mut account_ids = Vec::with_capacity(command.accounts.len());
            for account in &command.accounts {
                account_ids
                    .push(upsert_provider_account_in_transaction(&mut transaction, account).await?);
            }
            append_admin_audit_event_in_transaction(&mut transaction, command.audit, revision)
                .await?;
            Ok(ProviderAccountAdminImport {
                config_revision: revision,
                account_ids,
            })
        }
        .await;
        finish_admin_transaction(transaction, result, "provider account admin import").await
    }

    async fn rotate_provider_account(
        &self,
        command: RotateProviderAccount,
    ) -> StoreResult<ProviderAccountAdminRotation> {
        command.scope.validate()?;
        require_nonempty(ENTITY, "account_id", &command.profile.id)?;
        require_nonempty(ENTITY, "name", &command.profile.name)?;
        if let Some(identity) = &command.replacement_identity {
            require_nonempty(ENTITY, "upstream_user_id", identity.upstream_user_id())?;
            if let Some(account_id) = identity.upstream_account_id() {
                require_nonempty(ENTITY, "upstream_account_id", account_id)?;
            }
        }
        if command.profile.id != command.credential.account_id {
            return Err(invalid("rotated profile and credential account IDs differ"));
        }
        validate_credential_update(&command.credential)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin provider account admin rotation"))?;
        let result = async {
            let config_revision = bump_config_revision_in_transaction(&mut transaction).await?;
            let credential_revision = rotate_provider_account_in_transaction(
                &mut transaction,
                &command.scope,
                &command.profile,
                command.replacement_identity.as_ref(),
                &command.credential,
            )
            .await?;
            append_admin_audit_event_in_transaction(
                &mut transaction,
                command.audit,
                config_revision,
            )
            .await?;
            Ok(ProviderAccountAdminRotation {
                config_revision,
                credential_revision,
            })
        }
        .await;
        finish_admin_transaction(transaction, result, "provider account admin rotation").await
    }

    async fn set_provider_account_enabled_admin(
        &self,
        command: SetProviderAccountEnabled,
    ) -> StoreResult<Revision> {
        command.scope.validate()?;
        require_nonempty(ENTITY, "account_id", &command.account_id)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin provider account admin state change"))?;
        let result = async {
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            set_provider_account_enabled_in_transaction(
                &mut transaction,
                &command.scope,
                &command.account_id,
                command.enabled,
            )
            .await?;
            append_admin_audit_event_in_transaction(&mut transaction, command.audit, revision)
                .await?;
            Ok(revision)
        }
        .await;
        finish_admin_transaction(transaction, result, "provider account admin state change").await
    }

    async fn delete_provider_accounts_admin(
        &self,
        command: DeleteProviderAccounts,
    ) -> StoreResult<Revision> {
        command.scope.validate()?;
        validate_admin_account_ids(&command.account_ids)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin provider account admin deletion"))?;
        let result = async {
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            delete_provider_accounts_in_transaction(
                &mut transaction,
                &command.scope,
                &command.account_ids,
            )
            .await?;
            append_admin_audit_event_in_transaction(&mut transaction, command.audit, revision)
                .await?;
            Ok(revision)
        }
        .await;
        finish_admin_transaction(transaction, result, "provider account admin deletion").await
    }
}

pub(crate) async fn upsert_provider_account_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    account: &NewProviderAccount,
) -> StoreResult<String> {
    account.validate()?;
    let imported_id = sqlx::query_scalar::<_, String>(
        "insert into provider_accounts (
           id, provider_kind, name, email, upstream_user_id,
           upstream_account_id, plan_type, authentication_kind, provider_credentials_json, credential_revision,
           has_refresh_token, access_token_expires_at, next_refresh_at, enabled,
           availability, provider_quota_json,
           availability_observed_at, quota_observed_at, created_at, updated_at
         ) values (
           $1, $2, $3, $4, $5, $6, $7, $8, $9, 1, $10, $11, $12, $13,
           $14, null, $15, null, now(), greatest(now(), $15)
         )
         on conflict (
           provider_kind,
           upstream_user_id,
           (coalesce(upstream_account_id, ''))
         ) do update set
           name = excluded.name,
           email = excluded.email,
           plan_type = excluded.plan_type,
           authentication_kind = excluded.authentication_kind,
           provider_credentials_json = excluded.provider_credentials_json,
           credential_revision = provider_accounts.credential_revision + 1,
           has_refresh_token = excluded.has_refresh_token,
           access_token_expires_at = excluded.access_token_expires_at,
           next_refresh_at = excluded.next_refresh_at,
           enabled = excluded.enabled,
           availability = excluded.availability,
           provider_quota_json = null,
           availability_observed_at = excluded.availability_observed_at,
           quota_observed_at = null,
           updated_at = greatest(now(), excluded.availability_observed_at)
         returning id",
    )
    .bind(&account.id)
    .bind(&account.provider_kind)
    .bind(&account.name)
    .bind(&account.email)
    .bind(&account.upstream_user_id)
    .bind(&account.upstream_account_id)
    .bind(&account.plan_type)
    .bind(&account.authentication_kind)
    .bind(account.provider_credentials_json.as_value())
    .bind(account.has_refresh_token)
    .bind(account.access_token_expires_at)
    .bind(account.next_refresh_at)
    .bind(account.enabled)
    .bind(account.availability.as_str())
    .bind(account.availability_observed_at)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| {
        if error
            .as_database_error()
            .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
        {
            StoreError::Conflict {
                entity: ENTITY,
                id: account.id.clone(),
                kind: ConflictKind::InvalidTransition,
            }
        } else {
            postgres_unavailable("upsert provider account in admin transaction")
        }
    })?
    .ok_or_else(|| StoreError::Conflict {
        entity: ENTITY,
        id: account.id.clone(),
        kind: ConflictKind::InvalidTransition,
    })?;
    Ok(imported_id)
}

pub(crate) async fn rotate_provider_account_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &ProviderAccountAdminScope,
    profile: &UpdateProviderAccount,
    replacement_identity: Option<&ProviderAccountIdentity>,
    update: &ProviderCredentialUpdate,
) -> StoreResult<Revision> {
    let replace_identity = replacement_identity.is_some();
    let upstream_user_id = replacement_identity.map(ProviderAccountIdentity::upstream_user_id);
    let upstream_account_id =
        replacement_identity.and_then(ProviderAccountIdentity::upstream_account_id);
    let next = sqlx::query_scalar::<_, i64>(
        "update provider_accounts
         set name = $4,
             email = $5,
             plan_type = $6,
             provider_credentials_json = $7,
             credential_revision = credential_revision + 1,
             has_refresh_token = $8,
             access_token_expires_at = $9,
             next_refresh_at = $10,
             upstream_user_id = case when $11::boolean then $12::text else upstream_user_id end,
             upstream_account_id = case when $11::boolean then $13::text else upstream_account_id end,
             availability = case
                 when not enabled then availability
                 when availability <> 'quota_exhausted' then 'ready'
                 else availability
             end,
             availability_observed_at = case
                 when not enabled then availability_observed_at
                 when availability <> 'quota_exhausted' then now()
                 else availability_observed_at
             end,
             updated_at = now()
         where id = $1 and provider_kind = $2
           and credential_revision = $3
         returning credential_revision",
    )
    .bind(&update.account_id)
    .bind(&scope.provider_kind)
    .bind(to_i64(update.expected_revision.get())?)
    .bind(&profile.name)
    .bind(&profile.email)
    .bind(&profile.plan_type)
    .bind(update.provider_credentials_json.as_value())
    .bind(update.has_refresh_token)
    .bind(update.access_token_expires_at)
    .bind(update.next_refresh_at)
    .bind(replace_identity)
    .bind(upstream_user_id)
    .bind(upstream_account_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| {
        if error
            .as_database_error()
            .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
        {
            StoreError::Conflict {
                entity: ENTITY,
                id: update.account_id.clone(),
                kind: ConflictKind::InvalidTransition,
            }
        } else {
            postgres_unavailable("rotate provider account in admin transaction")
        }
    })?
    .ok_or_else(|| StoreError::Conflict {
        entity: ENTITY,
        id: update.account_id.clone(),
        kind: ConflictKind::StaleRevision,
    })?;
    Revision::new(to_u64(next)?)
}

pub(crate) async fn set_provider_account_enabled_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &ProviderAccountAdminScope,
    account_id: &str,
    enabled: bool,
) -> StoreResult<()> {
    let result = sqlx::query(
        "update provider_accounts set enabled = $3, updated_at = now()
         where id = $1 and provider_kind = $2",
    )
    .bind(account_id)
    .bind(&scope.provider_kind)
    .bind(enabled)
    .execute(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("set provider account state in admin transaction"))?;
    require_admin_account_changed(result.rows_affected(), account_id)
}

pub(crate) async fn delete_provider_accounts_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &ProviderAccountAdminScope,
    account_ids: &[String],
) -> StoreResult<()> {
    let deleted = sqlx::query_scalar::<_, String>(
        "delete from provider_accounts
         where id = any($1::text[]) and provider_kind = $2
         returning id",
    )
    .bind(account_ids)
    .bind(&scope.provider_kind)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("delete provider account in admin transaction"))?;
    let deleted = deleted.into_iter().collect::<BTreeSet<_>>();
    let expected = account_ids.iter().cloned().collect::<BTreeSet<_>>();
    if deleted == expected {
        Ok(())
    } else {
        Err(invalid(
            "all deleted accounts must exist and match Provider scope",
        ))
    }
}

pub(crate) fn validate_credential_update(update: &ProviderCredentialUpdate) -> StoreResult<()> {
    require_nonempty(ENTITY, "account_id", &update.account_id)?;
    validate_object_size(
        "provider_credentials_json",
        &update.provider_credentials_json,
        CREDENTIALS_MAX_BYTES,
    )?;
    if !update.has_refresh_token && update.next_refresh_at.is_some() {
        return Err(invalid("next_refresh_at requires a refresh token"));
    }
    Ok(())
}

pub(crate) fn require_admin_account_changed(
    rows_affected: u64,
    account_id: &str,
) -> StoreResult<()> {
    if rows_affected == 1 {
        Ok(())
    } else {
        Err(StoreError::NotFound {
            entity: ENTITY,
            id: account_id.to_owned(),
        })
    }
}

pub(crate) fn validate_admin_account_ids(account_ids: &[String]) -> StoreResult<()> {
    if account_ids.is_empty() || account_ids.len() > MAX_ADMIN_IMPORT_BATCH {
        return Err(invalid(
            "admin account selection must contain between 1 and 200 IDs",
        ));
    }
    let mut unique = BTreeSet::new();
    for account_id in account_ids {
        require_nonempty(ENTITY, "account_id", account_id)?;
        if !unique.insert(account_id.as_str()) {
            return Err(invalid("admin account selection contains duplicate IDs"));
        }
    }
    Ok(())
}

pub(crate) async fn finish_admin_transaction<T>(
    transaction: Transaction<'_, Postgres>,
    result: StoreResult<T>,
    operation: &'static str,
) -> StoreResult<T> {
    match result {
        Ok(value) => {
            transaction
                .commit()
                .await
                .map_err(|_| postgres_unavailable(operation))?;
            Ok(value)
        }
        Err(error) => {
            transaction
                .rollback()
                .await
                .map_err(|_| postgres_unavailable(operation))?;
            Err(error)
        }
    }
}
