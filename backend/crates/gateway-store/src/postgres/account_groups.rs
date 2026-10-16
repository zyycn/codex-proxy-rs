//! 跨 Provider 账号分组及成员关系的 PostgreSQL 持久化

use gateway_admin::model::audit::MutationAuditOperation;
use std::{collections::BTreeMap, str::FromStr as _};

use async_trait::async_trait;
use futures::future::BoxFuture;
use gateway_admin::{
    model::{
        MutationContext,
        account_groups::{
            AccountGroupAccountSummary, AccountGroupCapacity, AccountGroupColor,
            AccountGroupListQuery, AccountGroupMemberFact, AccountGroupMutation,
            AccountGroupOptionsPage, AccountGroupPage, AccountGroupRecord, AccountGroupRef,
            AccountGroupUsage, DeleteAccountGroup, NewAccountGroup, SetAccountGroupEnabled,
            UpdateAccountGroup,
        },
        observability::DecimalAmount,
    },
    ports::store::{AccountGroupStore, AdminStoreError, AdminStoreResult},
};
use gateway_core::{
    account::{AccountStatusFacts, FastMode},
    routing::AccountGroupId,
};
use sqlx::{PgPool, Postgres, QueryBuilder, Row as _, Transaction};

use crate::{
    ConflictKind, StoreError, StoreResult, admin_revision, admin_store_error, mutation_audit,
    postgres_unavailable,
};

use super::{
    ObservabilityQueryBudget, PgRuntimeSettingsRepository, RuntimeSettingsRepository,
    account_summary_from_row, append_admin_audit_event_in_transaction,
    bump_config_revision_in_transaction,
};

const ENTITY: &str = "account group";

/// 在同一事务中维护版本与审计的账号分组存储
#[derive(Clone)]
pub struct PgAccountGroupRepository {
    timezone: gateway_core::time::DeploymentTimeZone,
    pool: PgPool,
    query_budget: ObservabilityQueryBudget,
}

impl PgAccountGroupRepository {
    #[must_use]
    pub fn new(pool: PgPool, query_budget: ObservabilityQueryBudget) -> Self {
        Self {
            pool,
            query_budget,
            timezone: Default::default(),
        }
    }

    #[must_use]
    pub fn with_timezone(mut self, timezone: gateway_core::time::DeploymentTimeZone) -> Self {
        self.timezone = timezone;
        self
    }

    async fn current_revision(&self) -> AdminStoreResult<gateway_admin::model::Revision> {
        RuntimeSettingsRepository::load_runtime_settings(&PgRuntimeSettingsRepository::new(
            self.pool.clone(),
        ))
        .await
        .map_err(|error| admin_store_error(ENTITY, error))
        .and_then(|settings| admin_revision(settings.config_revision))
    }

    async fn required_record(&self, id: &AccountGroupId) -> AdminStoreResult<AccountGroupRecord> {
        let mut record = load_record(&self.pool, id.as_str())
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?
            .ok_or_else(|| not_found(id.as_str()))?;
        let costs = group_costs(
            &self.pool,
            &self.query_budget,
            &[id.as_str().to_owned()],
            self.timezone,
        )
        .await
        .map_err(|error| admin_store_error(ENTITY, error))?;
        if let Some(usage) = costs.get(id.as_str()) {
            record.usage = usage.clone();
        }
        Ok(record)
    }

    async fn mutate<F>(
        &self,
        audit: super::AdminAuditEvent,
        mutation: F,
    ) -> AdminStoreResult<gateway_admin::model::Revision>
    where
        F: for<'a> FnOnce(&'a mut Transaction<'_, Postgres>) -> BoxFuture<'a, StoreResult<()>>,
    {
        let mut transaction = self.pool.begin().await.map_err(|source| {
            admin_store_error(ENTITY, unavailable("begin account group mutation", source))
        })?;
        let result: StoreResult<_> = async {
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            mutation(&mut transaction).await?;
            append_admin_audit_event_in_transaction(&mut transaction, audit, revision).await?;
            Ok(revision)
        }
        .await;
        match result {
            Ok(revision) => {
                transaction.commit().await.map_err(|source| {
                    admin_store_error(ENTITY, unavailable("commit account group mutation", source))
                })?;
                admin_revision(revision)
            }
            Err(error) => {
                let error = match transaction.rollback().await {
                    Ok(()) => error,
                    Err(cleanup) => error.with_cleanup(cleanup),
                };
                Err(admin_store_error(ENTITY, error))
            }
        }
    }
}

#[async_trait]
impl AccountGroupStore for PgAccountGroupRepository {
    async fn list_account_groups(
        &self,
        query: AccountGroupListQuery,
    ) -> AdminStoreResult<AccountGroupPage> {
        validate_page_query(&query)?;
        let total = count_groups(&self.pool, &query)
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        let offset = u64::from(query.page.saturating_sub(1)) * u64::from(query.page_size.get());
        let mut statement = group_select();
        push_group_filter(&mut statement, &query);
        statement.push(" order by g.created_at desc, g.id desc limit ");
        statement.push_bind(i64::from(query.page_size.get()));
        statement.push(" offset ");
        statement.push_bind(i64::try_from(offset).map_err(|_| invalid_admin("page is too large"))?);
        let rows = statement
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|source| {
                admin_store_error(ENTITY, unavailable("list account groups", source))
            })?;
        let mut items = rows
            .iter()
            .map(group_record)
            .collect::<StoreResult<Vec<_>>>()
            .map_err(|error| admin_store_error(ENTITY, error))?;
        let group_ids = items
            .iter()
            .map(|record| record.id.as_str().to_owned())
            .collect::<Vec<_>>();
        let costs = group_costs(&self.pool, &self.query_budget, &group_ids, self.timezone)
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        for record in &mut items {
            if let Some(usage) = costs.get(record.id.as_str()) {
                record.usage = usage.clone();
            }
        }
        Ok(AccountGroupPage {
            config_revision: self.current_revision().await?,
            items,
            total,
            page: query.page,
            page_size: query.page_size.get(),
        })
    }

    async fn list_account_group_options(
        &self,
        query: AccountGroupListQuery,
    ) -> AdminStoreResult<AccountGroupOptionsPage> {
        validate_page_query(&query)?;
        let total = count_groups(&self.pool, &query)
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        let offset = u64::from(query.page.saturating_sub(1)) * u64::from(query.page_size.get());
        let mut statement = QueryBuilder::<Postgres>::new(
            "select g.id, g.name, g.color, g.enabled from account_groups g where true",
        );
        push_group_filter(&mut statement, &query);
        statement.push(" order by g.created_at desc, g.id desc limit ");
        statement.push_bind(i64::from(query.page_size.get()));
        statement.push(" offset ");
        statement.push_bind(i64::try_from(offset).map_err(|_| invalid_admin("page is too large"))?);
        let rows = statement
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|source| {
                admin_store_error(ENTITY, unavailable("list account group options", source))
            })?;
        let items = rows
            .iter()
            .map(group_ref)
            .collect::<StoreResult<Vec<_>>>()
            .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok(AccountGroupOptionsPage {
            config_revision: self.current_revision().await?,
            items,
            total,
            page: query.page,
            page_size: query.page_size.get(),
        })
    }

    async fn load_account_group_members(
        &self,
        group_ids: &[AccountGroupId],
    ) -> AdminStoreResult<Vec<AccountGroupMemberFact>> {
        if group_ids.is_empty() {
            return Ok(Vec::new());
        }
        let group_ids = group_ids
            .iter()
            .map(|group_id| group_id.as_str().to_owned())
            .collect::<Vec<_>>();
        let rows = sqlx::query(
            "select coalesce(proxy.enabled, true) as outbound_proxy_enabled, proxy.auto_location, proxy.detected_location_json, proxy.location_country, proxy.location_region, proxy.location_city, proxy.location_timezone, membership.account_group_id,
                    account.id, account.provider_kind, account.name, account.notes, account.email,
                    account.upstream_user_id, account.upstream_account_id, account.plan_type,
                    account.authentication_kind, account.credential_revision, account.outbound_proxy_url,
                    account.has_refresh_token, account.access_token_expires_at,
                    account.next_refresh_at, account.enabled, account.concurrency_limit,
                    account.weight, account.model_access_json, account.credential_state, account.quota_access_state,
                    account.quota_evidence, account.quota_access_observed_at,
                    account.quota_reset_at, account.last_error_reason,
                    account.last_error_message, account.credential_observed_at,
                    account.created_at, account.updated_at,
                    settings.max_concurrent_per_account
               from account_group_accounts membership
               join provider_accounts account on account.id = membership.provider_account_id
               left join outbound_proxies proxy on proxy.id = account.outbound_proxy_id
               cross join runtime_settings settings
              where settings.id = 1
                and membership.account_group_id = any($1::text[])
              order by membership.account_group_id, membership.provider_account_id",
        )
        .bind(group_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| admin_store_error(ENTITY, unavailable("load account group members", source)))?;
        rows.into_iter()
            .map(|row| {
                let group_id = AccountGroupId::new(
                    row.try_get::<String, _>("account_group_id")
                        .map_err(|source| invalid("invalid group ID").with_source(source))?,
                )
                .map_err(|source| invalid("invalid group ID").with_source(source))?;
                let default_slots = u64::try_from(
                    row.try_get::<i64, _>("max_concurrent_per_account")
                        .map_err(|source| {
                            invalid("invalid default account concurrency").with_source(source)
                        })?,
                )
                .map_err(|source| {
                    invalid("invalid default account concurrency").with_source(source)
                })?;
                let account = account_summary_from_row(row)?;
                Ok(AccountGroupMemberFact {
                    group_id,
                    account_id: account.id,
                    status: AccountStatusFacts {
                        enabled: account.enabled,
                        credential_state: account.credential_state,
                        access_token_expires_at: account.access_token_expires_at.map(Into::into),
                        quota: account.quota,
                        cooldown: None,
                        last_error_reason: account.last_error_reason,
                        last_error_message: account.last_error_message,
                    },
                    total_slots: account.concurrency_limit.map_or_else(
                        || (default_slots > 0).then_some(default_slots),
                        |limit| Some(u64::from(limit.get())),
                    ),
                })
            })
            .collect::<StoreResult<Vec<_>>>()
            .map_err(|error| admin_store_error(ENTITY, error))
    }

    async fn create_account_group(
        &self,
        command: NewAccountGroup,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation> {
        let id = command.id.clone();
        let audit = mutation_audit(
            context,
            MutationAuditOperation::AccountGroupCreate,
            id.as_str(),
            vec![
                "name".to_owned(),
                "description".to_owned(),
                "fast_mode".to_owned(),
                "allow_image_generation".to_owned(),
            ],
        );
        let revision = self
            .mutate(audit, |transaction| {
                Box::pin(async move {
                    insert_account_group_in_transaction(transaction, &command).await?;
                    Ok(())
                })
            })
            .await?;
        Ok(AccountGroupMutation {
            config_revision: revision,
            id: id.clone(),
            record: Some(self.required_record(&id).await?),
        })
    }

    async fn update_account_group(
        &self,
        command: UpdateAccountGroup,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation> {
        validate_group_fields(&command.name, command.description.as_deref())?;
        let id = command.id.clone();
        let audit = mutation_audit(
            context,
            MutationAuditOperation::AccountGroupUpdate,
            id.as_str(),
            vec![
                "name".to_owned(),
                "description".to_owned(),
                "fast_mode".to_owned(),
                "allow_image_generation".to_owned(),
            ],
        );
        let revision = self
            .mutate(audit, |transaction| {
                Box::pin(async move {
                    let result = sqlx::query(
                        "update account_groups
                 set name = $2, description = $3, color = $4, fast_mode = coalesce($5, fast_mode), allow_image_generation = coalesce($6, allow_image_generation), updated_at = now()
                 where id = $1",
                    )
                    .bind(command.id.as_str())
                    .bind(command.name)
                    .bind(command.description)
                    .bind(command.color.as_str())
                    .bind(command.fast_mode.map(FastMode::as_str))
                    .bind(command.allow_image_generation)
                    .execute(&mut **transaction)
                    .await
                    .map_err(|error| map_group_write_error(error, command.id.as_str()))?;
                    require_one(result.rows_affected(), command.id.as_str())
                })
            })
            .await?;
        Ok(AccountGroupMutation {
            config_revision: revision,
            id: id.clone(),
            record: Some(self.required_record(&id).await?),
        })
    }

    async fn set_account_group_enabled(
        &self,
        command: SetAccountGroupEnabled,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation> {
        let id = command.id.clone();
        let audit = mutation_audit(
            context,
            MutationAuditOperation::AccountGroupEnabled {
                enabled: command.enabled,
            },
            id.as_str(),
            vec!["enabled".to_owned()],
        );
        let revision = self
            .mutate(audit, |transaction| {
                Box::pin(async move {
                    let result = sqlx::query(
                        "update account_groups set enabled = $2, updated_at = now() where id = $1",
                    )
                    .bind(command.id.as_str())
                    .bind(command.enabled)
                    .execute(&mut **transaction)
                    .await
                    .map_err(|source| unavailable("set account group state", source))?;
                    require_one(result.rows_affected(), command.id.as_str())
                })
            })
            .await?;
        Ok(AccountGroupMutation {
            config_revision: revision,
            id: id.clone(),
            record: Some(self.required_record(&id).await?),
        })
    }

    async fn delete_account_group(
        &self,
        command: DeleteAccountGroup,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation> {
        let id = command.id.clone();
        let audit = mutation_audit(
            context,
            MutationAuditOperation::AccountGroupDelete,
            id.as_str(),
            Vec::new(),
        );
        let revision = self
            .mutate(audit, |transaction| {
                Box::pin(async move {
                    let result = sqlx::query(
                        "delete from account_groups g
                         where g.id = $1
                           and not exists (
                             select 1 from client_api_key_groups k where k.account_group_id = g.id
                           )",
                    )
                    .bind(command.id.as_str())
                    .execute(&mut **transaction)
                    .await
                    .map_err(|source| unavailable("delete account group", source))?;
                    if result.rows_affected() == 1 {
                        return Ok(());
                    }
                    let exists = sqlx::query_scalar::<_, bool>(
                        "select exists(select 1 from account_groups where id = $1)",
                    )
                    .bind(command.id.as_str())
                    .fetch_one(&mut **transaction)
                    .await
                    .map_err(|source| unavailable("check account group delete conflict", source))?;
                    if exists {
                        Err(conflict(command.id.as_str()))
                    } else {
                        Err(not_found_store(command.id.as_str()))
                    }
                })
            })
            .await?;
        Ok(AccountGroupMutation {
            config_revision: revision,
            id,
            record: None,
        })
    }
}

fn group_select() -> QueryBuilder<Postgres> {
    QueryBuilder::new(
        "select g.id, g.name, g.description, g.color, g.enabled, g.fast_mode, g.allow_image_generation, g.created_at, g.updated_at,
                coalesce(members.member_count, 0)::bigint as member_count,
                coalesce(keys.client_key_count, 0)::bigint as client_key_count,
                coalesce(members.provider_counts, '{}'::jsonb) as provider_counts
         from account_groups g
         left join lateral (
           select coalesce(sum(counts.provider_count), 0)::bigint as member_count,
                  jsonb_object_agg(counts.provider_kind, counts.provider_count)
                    as provider_counts
           from (
             select a.provider_kind, count(*)::bigint as provider_count
             from account_group_accounts gm
             join provider_accounts a on a.id = gm.provider_account_id
             where gm.account_group_id = g.id
             group by a.provider_kind
           ) counts
         ) members on true
         left join lateral (
           select count(*)::bigint as client_key_count
           from client_api_key_groups kg
           where kg.account_group_id = g.id
         ) keys on true
         where true",
    )
}

fn push_group_filter(statement: &mut QueryBuilder<Postgres>, query: &AccountGroupListQuery) {
    if let Some(search) = &query.search {
        statement.push(" and lower(g.name) like ");
        statement.push_bind(format!("%{}%", search.to_lowercase()));
    }
    if let Some(enabled) = query.enabled {
        statement.push(" and g.enabled = ");
        statement.push_bind(enabled);
    }
}

async fn count_groups(pool: &PgPool, query: &AccountGroupListQuery) -> StoreResult<u64> {
    let mut statement =
        QueryBuilder::<Postgres>::new("select count(*)::bigint from account_groups g where true");
    if let Some(search) = &query.search {
        statement.push(" and lower(g.name) like ");
        statement.push_bind(format!("%{}%", search.to_lowercase()));
    }
    if let Some(enabled) = query.enabled {
        statement.push(" and g.enabled = ");
        statement.push_bind(enabled);
    }
    let count = statement
        .build_query_scalar::<i64>()
        .fetch_one(pool)
        .await
        .map_err(|source| unavailable("count account groups", source))?;
    u64::try_from(count)
        .map_err(|source| invalid("negative account group count").with_source(source))
}

async fn load_record(pool: &PgPool, id: &str) -> StoreResult<Option<AccountGroupRecord>> {
    let mut statement = group_select();
    statement.push(" and g.id = ");
    statement.push_bind(id.to_owned());
    statement
        .build()
        .fetch_optional(pool)
        .await
        .map_err(|source| unavailable("load account group", source))?
        .as_ref()
        .map(group_record)
        .transpose()
}

fn group_record(row: &sqlx::postgres::PgRow) -> StoreResult<AccountGroupRecord> {
    let provider_counts: serde_json::Value = row
        .try_get("provider_counts")
        .map_err(|source| invalid("invalid provider counts").with_source(source))?;
    let provider_counts = serde_json::from_value::<BTreeMap<String, u64>>(provider_counts)
        .map_err(|source| invalid("invalid provider counts").with_source(source))?;
    Ok(AccountGroupRecord {
        allow_image_generation: row
            .try_get("allow_image_generation")
            .map_err(|source| invalid("invalid image generation policy").with_source(source))?,
        fast_mode: FastMode::parse(
            row.try_get("fast_mode")
                .map_err(|source| invalid("invalid fast_mode").with_source(source))?,
        )
        .ok_or_else(|| invalid("invalid fast_mode"))?,
        id: AccountGroupId::new(
            row.try_get::<String, _>("id")
                .map_err(|source| invalid("invalid id").with_source(source))?,
        )
        .map_err(|source| invalid("invalid id").with_source(source))?,
        name: row
            .try_get("name")
            .map_err(|source| invalid("invalid name").with_source(source))?,
        description: row
            .try_get("description")
            .map_err(|source| invalid("invalid description").with_source(source))?,
        color: AccountGroupColor::parse(
            row.try_get::<String, _>("color")
                .map_err(|source| invalid("invalid color").with_source(source))?
                .as_str(),
        )
        .ok_or_else(|| invalid("invalid color"))?,
        enabled: row
            .try_get("enabled")
            .map_err(|source| invalid("invalid enabled").with_source(source))?,
        member_count: count_value(row, "member_count")?,
        provider_counts,
        client_key_count: count_value(row, "client_key_count")?,
        account_summary: AccountGroupAccountSummary {
            available: 0,
            limited: 0,
            total: 0,
        },
        capacity: AccountGroupCapacity {
            used_slots: None,
            total_slots: Some(0),
        },
        usage: AccountGroupUsage {
            today_usd: DecimalAmount::from_str("0")
                .map_err(|source| invalid("invalid zero cost").with_source(source))?,
            retained_total_usd: DecimalAmount::from_str("0")
                .map_err(|source| invalid("invalid zero cost").with_source(source))?,
        },
        created_at: row
            .try_get("created_at")
            .map_err(|source| invalid("invalid created_at").with_source(source))?,
        updated_at: row
            .try_get("updated_at")
            .map_err(|source| invalid("invalid updated_at").with_source(source))?,
    })
}

fn group_ref(row: &sqlx::postgres::PgRow) -> StoreResult<AccountGroupRef> {
    Ok(AccountGroupRef {
        id: AccountGroupId::new(
            row.try_get::<String, _>("id")
                .map_err(|source| invalid("invalid id").with_source(source))?,
        )
        .map_err(|source| invalid("invalid id").with_source(source))?,
        name: row
            .try_get("name")
            .map_err(|source| invalid("invalid name").with_source(source))?,
        color: AccountGroupColor::parse(
            row.try_get::<String, _>("color")
                .map_err(|source| invalid("invalid color").with_source(source))?
                .as_str(),
        )
        .ok_or_else(|| invalid("invalid color"))?,
        enabled: row
            .try_get("enabled")
            .map_err(|source| invalid("invalid enabled").with_source(source))?,
    })
}

async fn group_costs(
    pool: &PgPool,
    budget: &ObservabilityQueryBudget,
    group_ids: &[String],
    timezone: gateway_core::time::DeploymentTimeZone,
) -> StoreResult<BTreeMap<String, AccountGroupUsage>> {
    if group_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    budget
        .run(
            pool,
            "load account group costs",
            |mut connection| async move {
                super::observability::account_group_usage(&mut connection, group_ids, timezone)
                    .await
            },
        )
        .await
}

fn count_value(row: &sqlx::postgres::PgRow, field: &str) -> StoreResult<u64> {
    u64::try_from(
        row.try_get::<i64, _>(field)
            .map_err(|source| invalid("invalid count").with_source(source))?,
    )
    .map_err(|source| invalid("negative count").with_source(source))
}

fn validate_page_query(query: &AccountGroupListQuery) -> AdminStoreResult<()> {
    if query.page == 0 {
        return Err(invalid_admin("page must be positive"));
    }
    if query.search.as_deref().is_some_and(|search| {
        search.trim().is_empty() || search.len() > 256 || search.chars().any(char::is_control)
    }) {
        return Err(invalid_admin("invalid search"));
    }
    Ok(())
}

fn validate_group_fields(name: &str, description: Option<&str>) -> AdminStoreResult<()> {
    gateway_admin::model::account_groups::validate_group_fields(name, description)
        .map_err(|_| invalid_admin("invalid account group fields"))
}

fn require_one(rows: u64, id: &str) -> StoreResult<()> {
    if rows == 1 {
        Ok(())
    } else {
        Err(not_found_store(id))
    }
}

fn map_group_write_error(error: sqlx::Error, id: &str) -> StoreError {
    if error
        .as_database_error()
        .is_some_and(|database| database.is_unique_violation())
    {
        conflict(id)
    } else {
        unavailable("write account group", error)
    }
}

fn not_found(id: &str) -> AdminStoreError {
    admin_store_error(ENTITY, not_found_store(id))
}

fn not_found_store(id: &str) -> StoreError {
    StoreError::NotFound {
        source: None,
        entity: ENTITY,
        id: id.to_owned(),
    }
}

fn conflict(id: &str) -> StoreError {
    StoreError::Conflict {
        source: None,
        entity: ENTITY,
        id: id.to_owned(),
        kind: ConflictKind::InvalidTransition,
    }
}

fn invalid_admin(message: &str) -> AdminStoreError {
    admin_store_error(ENTITY, invalid(message))
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        source: None,
        entity: ENTITY,
        message: message.to_owned(),
    }
}

fn unavailable(
    message: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> StoreError {
    postgres_unavailable(message, source)
}

/// 原生管理与插件自有分组共用字段校验和写入规则
pub(crate) async fn insert_account_group_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    command: &NewAccountGroup,
) -> StoreResult<()> {
    validate_group_fields(&command.name, command.description.as_deref())
        .map_err(|source| invalid("invalid account group fields").with_source(source))?;
    sqlx::query(
        "insert into account_groups
         (id, name, description, color, fast_mode, allow_image_generation, enabled, created_at, updated_at)
         values ($1, $2, $3, $4, $5, $6, true, now(), now())",
    )
    .bind(command.id.as_str())
    .bind(&command.name)
    .bind(&command.description)
    .bind(command.color.as_str())
    .bind(command.fast_mode.as_str())
    .bind(command.allow_image_generation)
    .execute(&mut **transaction)
    .await
    .map_err(|error| map_group_write_error(error, command.id.as_str()))?;
    Ok(())
}
