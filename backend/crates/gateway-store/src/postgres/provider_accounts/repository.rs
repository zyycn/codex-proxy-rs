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
        quota: JsonObject,
        observed_at: DateTime<Utc>,
        state: QuotaState,
    ) -> StoreResult<bool>;
    async fn touch_provider_quota_observation(
        &self,
        account_id: &str,
        expected_revision: Revision,
        observed_at: DateTime<Utc>,
    ) -> StoreResult<bool>;
    async fn apply_provider_quota_access(
        &self,
        account_id: &str,
        expected_revision: Revision,
        state: QuotaState,
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

    async fn batch_update_provider_accounts_admin(
        &self,
        command: BatchUpdateProviderAccountsAdmin,
    ) -> StoreResult<Revision>;

    async fn recover_provider_account_admin(
        &self,
        command: RecoverProviderAccount,
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
                    access_token_expires_at, next_refresh_at, enabled, concurrency_limit, weight, credential_state,
                    credential_observed_at, quota_access_state, quota_evidence,
                    quota_access_observed_at, quota_reset_at,
                    quota_observed_at, last_error_reason, last_error_message, created_at, updated_at
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
        let credential_state = if account.upstream_user_id.is_some() {
            account.credential_state
        } else {
            CredentialState::Unknown
        };
        sqlx::query(
            "insert into provider_accounts (
               id, provider_kind, name, email, upstream_user_id,
               upstream_account_id, plan_type, authentication_kind, provider_credentials_json, credential_revision,
               has_refresh_token, access_token_expires_at, next_refresh_at, enabled,
               concurrency_limit, weight, credential_state, provider_quota_json,
               credential_observed_at, quota_access_observed_at, quota_observed_at, created_at, updated_at
             ) values (
               $1, $2, $3, $4, $5, $6, $7, $8, $9, 1, $10, $11, $12, $13,
               $14, $15, $16, null, $17, null, null, now(), greatest(now(), $17)
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
        .bind(account.concurrency_limit.map(|limit| i64::from(limit.get())))
        .bind(i16::try_from(account.weight.get()).map_err(|_| invalid("invalid weight"))?)
        .bind(credential_state.as_str())
        .bind(account.credential_observed_at)
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
             set credential_state = case
                     when enabled and upstream_user_id is not null then $3
                     else credential_state
                 end,
                 credential_observed_at = case
                     when enabled and upstream_user_id is not null then $4
                     else credential_observed_at
                 end,
                 last_error_reason = case
                     when enabled and upstream_user_id is not null then $5
                     else last_error_reason
                 end,
                 last_error_message = case
                     when enabled and upstream_user_id is not null then $6
                     else last_error_message
                 end,
                 updated_at = case when enabled then greatest(now(), $4) else updated_at end
             where id = $1 and credential_revision = $2
               and (credential_observed_at is null or credential_observed_at <= $4)",
        )
        .bind(update.account_id)
        .bind(to_i64(update.expected_revision.get())?)
        .bind(update.credential_state.as_str())
        .bind(update.credential_observed_at)
        .bind(update.error_reason.map(AccountErrorReason::as_str))
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
        quota: JsonObject,
        observed_at: DateTime<Utc>,
        state: QuotaState,
    ) -> StoreResult<bool> {
        require_nonempty(ENTITY, "account_id", account_id)?;
        validate_object_size("provider_quota_json", &quota, QUOTA_MAX_BYTES)?;
        let access_observed_at = state.observed_at().map(DateTime::<Utc>::from);
        let result = sqlx::query(
            "update provider_accounts
             set provider_quota_json = $3, quota_observed_at = $4,
                 quota_access_state = case
                   when $7::timestamptz is not null
                     and (quota_access_observed_at is null or quota_access_observed_at <= $7)
                   then $5 else quota_access_state end,
                 quota_evidence = case
                   when $7::timestamptz is not null
                     and (quota_access_observed_at is null or quota_access_observed_at <= $7)
                   then $6 else quota_evidence end,
                 quota_access_observed_at = case
                   when $7::timestamptz is not null
                     and (quota_access_observed_at is null or quota_access_observed_at <= $7)
                   then $7 else quota_access_observed_at end,
                 quota_reset_at = case
                   when $7::timestamptz is not null
                     and (quota_access_observed_at is null or quota_access_observed_at <= $7)
                   then $8 else quota_reset_at end,
                 updated_at = greatest(now(), $4)
             where id = $1 and credential_revision = $2
               and (quota_observed_at is null or quota_observed_at <= $4)",
        )
        .bind(account_id)
        .bind(to_i64(expected_revision.get())?)
        .bind(quota.as_value())
        .bind(observed_at)
        .bind(state.access().as_str())
        .bind(state.evidence().map(QuotaEvidence::as_str))
        .bind(access_observed_at)
        .bind(state.reset_at().map(DateTime::<Utc>::from))
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("compare and swap provider quota"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn apply_provider_quota_access(
        &self,
        account_id: &str,
        expected_revision: Revision,
        state: QuotaState,
    ) -> StoreResult<bool> {
        require_nonempty(ENTITY, "account_id", account_id)?;
        let observed_at = state
            .observed_at()
            .map(DateTime::<Utc>::from)
            .ok_or_else(|| invalid("quota access change requires observed_at"))?;
        let result = sqlx::query(
            "update provider_accounts
             set quota_access_observed_at = $3, quota_access_state = $4,
                 quota_evidence = $5, quota_reset_at = $6,
                 updated_at = greatest(now(), $3)
             where id = $1 and credential_revision = $2
               and (quota_access_observed_at is null or quota_access_observed_at <= $3)",
        )
        .bind(account_id)
        .bind(to_i64(expected_revision.get())?)
        .bind(observed_at)
        .bind(state.access().as_str())
        .bind(state.evidence().map(QuotaEvidence::as_str))
        .bind(state.reset_at().map(DateTime::<Utc>::from))
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("apply provider quota access"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn touch_provider_quota_observation(
        &self,
        account_id: &str,
        expected_revision: Revision,
        observed_at: DateTime<Utc>,
    ) -> StoreResult<bool> {
        require_nonempty(ENTITY, "account_id", account_id)?;
        let result = sqlx::query(
            "update provider_accounts
             set quota_observed_at = $3
             where id = $1 and credential_revision = $2
               and provider_quota_json is not null
               and (quota_observed_at is null or quota_observed_at <= $3)",
        )
        .bind(account_id)
        .bind(to_i64(expected_revision.get())?)
        .bind(observed_at)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("touch provider quota observation"))?;
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

    async fn batch_update_provider_accounts_admin(
        &self,
        command: BatchUpdateProviderAccountsAdmin,
    ) -> StoreResult<Revision> {
        validate_batch_update_account_ids(&command.account_ids)?;
        validate_batch_update_group_ids(&command.group_ids)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin provider account admin state change"))?;
        let result = async {
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            update_provider_accounts_scheduling_in_transaction(
                &mut transaction,
                &command.account_ids,
                command.enabled,
                command.concurrency_limit,
                command.weight,
            )
            .await?;
            replace_account_group_assignments_in_transaction(
                &mut transaction,
                &command.account_ids,
                &command.group_ids,
            )
            .await?;
            append_admin_audit_event_in_transaction(&mut transaction, command.audit, revision)
                .await?;
            Ok(revision)
        }
        .await;
        finish_admin_transaction(transaction, result, "provider account admin state change").await
    }

    async fn recover_provider_account_admin(
        &self,
        command: RecoverProviderAccount,
    ) -> StoreResult<Revision> {
        validate_admin_account_ids(std::slice::from_ref(&command.account_id))?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin provider account admin recovery"))?;
        let result = async {
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            let recovered = sqlx::query_scalar::<_, String>(
                "update provider_accounts
                 set enabled = true,
                     credential_state = 'ready',
                     credential_observed_at = now(),
                     access_token_expires_at = case
                         when access_token_expires_at <= now() then null
                         else access_token_expires_at
                     end,
                     provider_quota_json = null,
                     quota_observed_at = null,
                     quota_access_state = 'allowed',
                     quota_evidence = null,
                     quota_access_observed_at = now(),
                     quota_reset_at = null,
                     last_error_reason = null,
                     last_error_message = null,
                     updated_at = now()
                 where id = $1
                 returning id",
            )
            .bind(&command.account_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|_| postgres_unavailable("recover provider account state"))?
            .ok_or_else(|| StoreError::NotFound {
                entity: ENTITY,
                id: command.account_id.clone(),
            })?;
            append_admin_audit_event_in_transaction(&mut transaction, command.audit, revision)
                .await?;
            Ok((revision, recovered))
        }
        .await;
        finish_admin_transaction(transaction, result, "provider account admin recovery")
            .await
            .map(|(revision, _)| revision)
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

async fn replace_account_group_assignments_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    account_ids: &[String],
    group_ids: &[AccountGroupId],
) -> StoreResult<()> {
    let group_ids = group_ids
        .iter()
        .map(|group_id| group_id.as_str().to_owned())
        .collect::<Vec<_>>();
    if !group_ids.is_empty() {
        let known_group_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint from account_groups where id = any($1::text[])",
        )
        .bind(&group_ids)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|_| postgres_unavailable("validate account group assignment"))?;
        if usize::try_from(known_group_count).ok() != Some(group_ids.len()) {
            return Err(StoreError::NotFound {
                entity: "account group",
                id: "one or more group IDs".to_owned(),
            });
        }
    }
    sqlx::query("delete from account_group_accounts where provider_account_id = any($1::text[])")
        .bind(account_ids)
        .execute(&mut **transaction)
        .await
        .map_err(|_| postgres_unavailable("clear account group assignments"))?;
    if group_ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "insert into account_group_accounts
         (account_group_id, provider_account_id, created_at)
         select group_id, account_id, now()
         from unnest($1::text[]) group_id
         cross join unnest($2::text[]) account_id",
    )
    .bind(group_ids)
    .bind(account_ids)
    .execute(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("assign accounts to groups"))?;
    Ok(())
}

pub(crate) async fn upsert_provider_account_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    account: &NewProviderAccount,
) -> StoreResult<String> {
    account.validate()?;
    let credential_state = if account.upstream_user_id.is_some() {
        account.credential_state
    } else {
        CredentialState::Unknown
    };
    let imported_id = sqlx::query_scalar::<_, String>(
        "insert into provider_accounts (
           id, provider_kind, name, email, upstream_user_id,
           upstream_account_id, plan_type, authentication_kind, provider_credentials_json, credential_revision,
           has_refresh_token, access_token_expires_at, next_refresh_at, enabled,
           concurrency_limit, weight, credential_state, provider_quota_json,
           credential_observed_at, quota_access_observed_at, quota_observed_at, created_at, updated_at
         ) values (
           $1, $2, $3, $4, $5, $6, $7, $8, $9, 1, $10, $11, $12, $13,
           $14, $15, $16, null, $17, null, null, now(), greatest(now(), $17)
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
           credential_state = excluded.credential_state,
           provider_quota_json = null,
           quota_access_state = 'unknown',
           quota_evidence = null,
           quota_access_observed_at = null,
           quota_reset_at = null,
           credential_observed_at = excluded.credential_observed_at,
           quota_observed_at = null,
           last_error_reason = null,
           last_error_message = null,
           updated_at = greatest(now(), excluded.credential_observed_at)
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
    .bind(account.concurrency_limit.map(|limit| i64::from(limit.get())))
    .bind(i16::try_from(account.weight.get()).map_err(|_| invalid("invalid weight"))?)
    .bind(credential_state.as_str())
    .bind(account.credential_observed_at)
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
             credential_state = case
                 when not enabled then credential_state
                 when coalesce($12::text, upstream_user_id) is not null then 'ready'
                 else 'unknown'
             end,
             credential_observed_at = case
                 when not enabled then credential_observed_at
                 else now()
             end,
             last_error_reason = case when enabled then null else last_error_reason end,
             last_error_message = case when enabled then null else last_error_message end,
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

pub(crate) async fn update_provider_accounts_scheduling_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    account_ids: &[String],
    enabled: bool,
    concurrency_limit: Option<AccountConcurrencyLimit>,
    weight: AccountWeight,
) -> StoreResult<()> {
    let updated = sqlx::query_scalar::<_, String>(
        "update provider_accounts
         set enabled = $2, concurrency_limit = $3, weight = $4, updated_at = now()
         where id = any($1::text[])
         returning id",
    )
    .bind(account_ids)
    .bind(enabled)
    .bind(concurrency_limit.map(|limit| i64::from(limit.get())))
    .bind(i16::try_from(weight.get()).map_err(|_| invalid("invalid weight"))?)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("set provider accounts state in admin transaction"))?
    .into_iter()
    .collect::<BTreeSet<_>>();
    let expected = account_ids.iter().cloned().collect::<BTreeSet<_>>();
    if updated == expected {
        Ok(())
    } else {
        Err(StoreError::NotFound {
            entity: ENTITY,
            id: "one or more provider account IDs".to_owned(),
        })
    }
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

fn validate_batch_update_account_ids(account_ids: &[String]) -> StoreResult<()> {
    const MAX_BATCH_UPDATE_ACCOUNTS: usize = 1000;
    if account_ids.is_empty() || account_ids.len() > MAX_BATCH_UPDATE_ACCOUNTS {
        return Err(invalid(
            "account batch update must contain between 1 and 1000 IDs",
        ));
    }
    let mut unique = BTreeSet::new();
    for account_id in account_ids {
        require_nonempty(ENTITY, "account_id", account_id)?;
        if !unique.insert(account_id.as_str()) {
            return Err(invalid("account batch update contains duplicate IDs"));
        }
    }
    Ok(())
}

fn validate_batch_update_group_ids(group_ids: &[AccountGroupId]) -> StoreResult<()> {
    const MAX_BATCH_UPDATE_GROUPS: usize = 1000;
    if group_ids.len() > MAX_BATCH_UPDATE_GROUPS {
        return Err(invalid("account batch update contains too many group IDs"));
    }
    let mut unique = BTreeSet::new();
    if group_ids
        .iter()
        .any(|group_id| !unique.insert(group_id.as_str()))
    {
        return Err(invalid("account batch update contains duplicate group IDs"));
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
