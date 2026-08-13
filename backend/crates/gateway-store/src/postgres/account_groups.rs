//! PostgreSQL owner for provider-neutral account groups and memberships.

use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr as _,
    sync::Arc,
};

use async_trait::async_trait;
use futures::future::BoxFuture;
use gateway_admin::{
    model::{
        MutationContext,
        account_groups::{
            AccountGroupAccountSummary, AccountGroupCapacity, AccountGroupColor,
            AccountGroupListQuery, AccountGroupMember, AccountGroupMembers, AccountGroupMutation,
            AccountGroupPage, AccountGroupRecord, AccountGroupUsage, DeleteAccountGroup,
            NewAccountGroup, SetAccountGroupEnabled, UpdateAccountGroup,
        },
        observability::DecimalAmount,
    },
    ports::store::{AccountGroupStore, AdminStoreError, AdminStoreResult},
};
use gateway_core::{
    engine::credential::AccountStatus,
    provider_ports::ProviderCooldownPort,
    routing::{AccountGroupId, ProviderKind},
};
use sqlx::{PgPool, Postgres, QueryBuilder, Row as _, Transaction};

use crate::redis::CredentialLeaseRepository as _;
use crate::{
    ConflictKind, StoreError, StoreResult, admin_revision, admin_store_error, mutation_audit,
    postgres_unavailable,
};

use super::{
    PgProviderAccountRepository, PgRuntimeSettingsRepository, ProviderAccountRepository,
    RuntimeSettingsRepository, account_status_projection, append_admin_audit_event_in_transaction,
    bump_config_revision_in_transaction, load_rate_limited_until,
};

const ENTITY: &str = "account group";

/// Account group store with transactional revision and audit ownership.
#[derive(Clone)]
pub struct PgAccountGroupRepository {
    pool: PgPool,
    runtime_signals: Option<crate::redis::RedisCredentialLeaseRepository>,
    cooldowns: Option<Arc<dyn ProviderCooldownPort>>,
}

impl PgAccountGroupRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            pool,
            runtime_signals: None,
            cooldowns: None,
        }
    }

    #[must_use]
    pub fn with_runtime_state(
        pool: PgPool,
        runtime_signals: crate::redis::RedisCredentialLeaseRepository,
        cooldowns: Arc<dyn ProviderCooldownPort>,
    ) -> Self {
        Self {
            pool,
            runtime_signals: Some(runtime_signals),
            cooldowns: Some(cooldowns),
        }
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
        let record = load_record(&self.pool, id.as_str())
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?
            .ok_or_else(|| not_found(id.as_str()))?;
        let mut records = vec![record];
        self.enrich_records(&mut records).await?;
        records
            .pop()
            .ok_or_else(|| invalid_admin("account group record disappeared"))
    }

    async fn enrich_records(&self, records: &mut [AccountGroupRecord]) -> AdminStoreResult<()> {
        enrich_group_records(
            &self.pool,
            self.runtime_signals.as_ref(),
            self.cooldowns.as_deref(),
            records,
        )
        .await
        .map_err(|error| admin_store_error(ENTITY, error))
    }

    async fn mutate<F>(
        &self,
        audit: super::AdminAuditEvent,
        mutation: F,
    ) -> AdminStoreResult<gateway_admin::model::Revision>
    where
        F: for<'a> FnOnce(&'a mut Transaction<'_, Postgres>) -> BoxFuture<'a, StoreResult<()>>,
    {
        let mut transaction =
            self.pool.begin().await.map_err(|_| {
                admin_store_error(ENTITY, unavailable("begin account group mutation"))
            })?;
        let result = async {
            let revision = bump_config_revision_in_transaction(&mut transaction).await?;
            mutation(&mut transaction).await?;
            append_admin_audit_event_in_transaction(&mut transaction, audit, revision).await?;
            Ok(revision)
        }
        .await;
        match result {
            Ok(revision) => {
                transaction.commit().await.map_err(|_| {
                    admin_store_error(ENTITY, unavailable("commit account group mutation"))
                })?;
                admin_revision(revision)
            }
            Err(error) => {
                transaction.rollback().await.map_err(|_| {
                    admin_store_error(ENTITY, unavailable("rollback account group mutation"))
                })?;
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
            .map_err(|_| admin_store_error(ENTITY, unavailable("list account groups")))?;
        let mut items = rows
            .iter()
            .map(group_record)
            .collect::<StoreResult<Vec<_>>>()
            .map_err(|error| admin_store_error(ENTITY, error))?;
        self.enrich_records(&mut items).await?;
        Ok(AccountGroupPage {
            config_revision: self.current_revision().await?,
            items,
            total,
            page: query.page,
            page_size: query.page_size.get(),
        })
    }

    async fn account_group_members(
        &self,
        id: &AccountGroupId,
    ) -> AdminStoreResult<AccountGroupMembers> {
        self.required_record(id).await?;
        let rows = sqlx::query(
            "select a.id, a.name, a.provider_kind, a.email, a.enabled
             from account_group_accounts m
             join provider_accounts a on a.id = m.provider_account_id
             where m.account_group_id = $1
             order by a.provider_kind, lower(a.name), a.id",
        )
        .bind(id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(|_| admin_store_error(ENTITY, unavailable("load account group members")))?;
        let items = rows
            .iter()
            .map(group_member)
            .collect::<StoreResult<Vec<_>>>()
            .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok(AccountGroupMembers {
            config_revision: self.current_revision().await?,
            id: id.clone(),
            total: u64::try_from(items.len())
                .map_err(|_| invalid_admin("member count overflow"))?,
            items,
        })
    }

    async fn create_account_group(
        &self,
        command: NewAccountGroup,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation> {
        validate_group_fields(&command.name, command.description.as_deref())?;
        let id = command.id.clone();
        let audit = mutation_audit(
            context,
            "create",
            "account_group",
            id.as_str(),
            vec!["name".to_owned(), "description".to_owned()],
        );
        let revision = self
            .mutate(audit, |transaction| {
                Box::pin(async move {
                    sqlx::query(
                        "insert into account_groups
                         (id, name, description, color, enabled, created_at, updated_at)
                         values ($1, $2, $3, $4, true, now(), now())",
                    )
                    .bind(command.id.as_str())
                    .bind(command.name)
                    .bind(command.description)
                    .bind(command.color.as_str())
                    .execute(&mut **transaction)
                    .await
                    .map_err(|error| map_group_write_error(error, command.id.as_str()))?;
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
            "update",
            "account_group",
            id.as_str(),
            vec!["name".to_owned(), "description".to_owned()],
        );
        let revision = self
            .mutate(audit, |transaction| {
                Box::pin(async move {
                    let result = sqlx::query(
                        "update account_groups
                 set name = $2, description = $3, color = $4, updated_at = now()
                 where id = $1",
                    )
                    .bind(command.id.as_str())
                    .bind(command.name)
                    .bind(command.description)
                    .bind(command.color.as_str())
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
            if command.enabled { "enable" } else { "disable" },
            "account_group",
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
                    .map_err(|_| unavailable("set account group state"))?;
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
        let audit = mutation_audit(context, "delete", "account_group", id.as_str(), Vec::new());
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
                    .map_err(|_| unavailable("delete account group"))?;
                    if result.rows_affected() == 1 {
                        return Ok(());
                    }
                    let exists = sqlx::query_scalar::<_, bool>(
                        "select exists(select 1 from account_groups where id = $1)",
                    )
                    .bind(command.id.as_str())
                    .fetch_one(&mut **transaction)
                    .await
                    .map_err(|_| unavailable("check account group delete conflict"))?;
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
        "select g.id, g.name, g.description, g.color, g.enabled, g.created_at, g.updated_at,
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
        .map_err(|_| unavailable("count account groups"))?;
    u64::try_from(count).map_err(|_| invalid("negative account group count"))
}

async fn load_record(pool: &PgPool, id: &str) -> StoreResult<Option<AccountGroupRecord>> {
    let mut statement = group_select();
    statement.push(" and g.id = ");
    statement.push_bind(id.to_owned());
    statement
        .build()
        .fetch_optional(pool)
        .await
        .map_err(|_| unavailable("load account group"))?
        .as_ref()
        .map(group_record)
        .transpose()
}

fn group_record(row: &sqlx::postgres::PgRow) -> StoreResult<AccountGroupRecord> {
    let provider_counts: serde_json::Value = row
        .try_get("provider_counts")
        .map_err(|_| invalid("invalid provider counts"))?;
    let provider_counts = serde_json::from_value::<BTreeMap<String, u64>>(provider_counts)
        .map_err(|_| invalid("invalid provider counts"))?;
    Ok(AccountGroupRecord {
        id: AccountGroupId::new(
            row.try_get::<String, _>("id")
                .map_err(|_| invalid("invalid id"))?,
        )
        .map_err(|_| invalid("invalid id"))?,
        name: row.try_get("name").map_err(|_| invalid("invalid name"))?,
        description: row
            .try_get("description")
            .map_err(|_| invalid("invalid description"))?,
        color: AccountGroupColor::parse(
            row.try_get::<String, _>("color")
                .map_err(|_| invalid("invalid color"))?
                .as_str(),
        )
        .ok_or_else(|| invalid("invalid color"))?,
        enabled: row
            .try_get("enabled")
            .map_err(|_| invalid("invalid enabled"))?,
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
            total_slots: 0,
        },
        usage: AccountGroupUsage {
            today_usd: DecimalAmount::from_str("0").map_err(|_| invalid("invalid zero cost"))?,
            total_usd: DecimalAmount::from_str("0").map_err(|_| invalid("invalid zero cost"))?,
        },
        created_at: row
            .try_get("created_at")
            .map_err(|_| invalid("invalid created_at"))?,
        updated_at: row
            .try_get("updated_at")
            .map_err(|_| invalid("invalid updated_at"))?,
    })
}

async fn enrich_group_records(
    pool: &PgPool,
    runtime_signals: Option<&crate::redis::RedisCredentialLeaseRepository>,
    cooldowns: Option<&dyn ProviderCooldownPort>,
    records: &mut [AccountGroupRecord],
) -> StoreResult<()> {
    if records.is_empty() {
        return Ok(());
    }
    let settings = RuntimeSettingsRepository::load_runtime_settings(
        &PgRuntimeSettingsRepository::new(pool.clone()),
    )
    .await?;
    let accounts = PgProviderAccountRepository::new(pool.clone())
        .list_provider_accounts(None, true)
        .await?;
    let now = chrono::Utc::now();
    let cooldowns = load_rate_limited_until(cooldowns, &accounts, now.into()).await;
    let mut status_by_account = BTreeMap::new();
    let mut capacity_by_account = BTreeMap::new();
    for account in &accounts {
        let status =
            account_status_projection(account, now.into(), cooldowns.get(&account.id).copied())
                .status;
        status_by_account.insert(account.id.as_str(), status);
        capacity_by_account.insert(
            account.id.as_str(),
            u64::from(
                account
                    .concurrency_limit
                    .map_or(settings.max_concurrent_per_account, |limit| limit.get()),
            ),
        );
    }
    let group_ids = records
        .iter()
        .map(|record| record.id.as_str().to_owned())
        .collect::<Vec<_>>();
    let memberships = sqlx::query(
        "select account_group_id, provider_account_id
         from account_group_accounts
         where account_group_id = any($1::text[])",
    )
    .bind(&group_ids)
    .fetch_all(pool)
    .await
    .map_err(|_| unavailable("load account group availability"))?;
    let mut accounts_by_group = BTreeMap::<String, Vec<String>>::new();
    for row in memberships {
        accounts_by_group
            .entry(
                row.try_get("account_group_id")
                    .map_err(|_| invalid("invalid group ID"))?,
            )
            .or_default()
            .push(
                row.try_get("provider_account_id")
                    .map_err(|_| invalid("invalid account ID"))?,
            );
    }
    let normal_ids = accounts_by_group
        .values()
        .flatten()
        .filter(|account_id| {
            status_by_account.get(account_id.as_str()) == Some(&AccountStatus::Normal)
        })
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let used_by_account = if normal_ids.is_empty() {
        Some(BTreeMap::new())
    } else if let Some(runtime_signals) = runtime_signals {
        runtime_signals
            .credential_runtime_signals(&normal_ids)
            .await
            .ok()
            .map(|signals| {
                signals
                    .into_iter()
                    .map(|signal| (signal.resource_id, u64::from(signal.in_flight)))
                    .collect::<BTreeMap<_, _>>()
            })
    } else {
        None
    };
    let costs = group_costs(pool, &group_ids).await?;
    for record in records {
        let account_ids = accounts_by_group
            .get(record.id.as_str())
            .map_or(&[][..], Vec::as_slice);
        let available = account_ids
            .iter()
            .filter(|id| status_by_account.get(id.as_str()) == Some(&AccountStatus::Normal))
            .count() as u64;
        let total = u64::try_from(account_ids.len())
            .map_err(|_| invalid("group account count overflow"))?;
        record.account_summary = AccountGroupAccountSummary {
            available,
            limited: total.saturating_sub(available),
            total,
        };
        record.capacity = AccountGroupCapacity {
            used_slots: if available == 0 {
                Some(0)
            } else {
                used_by_account.as_ref().map(|used| {
                    account_ids.iter().fold(0_u64, |sum, id| {
                        sum.saturating_add(used.get(id).copied().unwrap_or(0))
                    })
                })
            },
            total_slots: account_ids.iter().fold(0_u64, |total, account_id| {
                if status_by_account.get(account_id.as_str()) == Some(&AccountStatus::Normal) {
                    total.saturating_add(
                        capacity_by_account
                            .get(account_id.as_str())
                            .copied()
                            .unwrap_or(0),
                    )
                } else {
                    total
                }
            }),
        };
        if let Some(usage) = costs.get(record.id.as_str()) {
            record.usage = usage.clone();
        }
    }
    Ok(())
}

async fn group_costs(
    pool: &PgPool,
    group_ids: &[String],
) -> StoreResult<BTreeMap<String, AccountGroupUsage>> {
    let rows = sqlx::query(
        "select gm.account_group_id,
                coalesce(sum(mr.cost_amount) filter (
                  where mr.started_at >= date_trunc('day', now() at time zone 'Asia/Shanghai')
                    at time zone 'Asia/Shanghai'
                ), 0)::text as today_usd,
                coalesce(sum(mr.cost_amount), 0)::text as total_usd
         from account_group_accounts gm
         left join model_requests mr
           on mr.provider_account_ref = gm.provider_account_id
          and mr.outcome = 'succeeded'
          and mr.downstream_committed_at is not null
          and mr.client_status_code between 200 and 399
          and mr.cost_currency = 'USD'
          and mr.cost_amount is not null
         where gm.account_group_id = any($1::text[])
         group by gm.account_group_id",
    )
    .bind(group_ids)
    .fetch_all(pool)
    .await
    .map_err(|_| unavailable("load account group costs"))?;
    rows.into_iter()
        .map(|row| {
            let group_id = row
                .try_get("account_group_id")
                .map_err(|_| invalid("invalid group ID"))?;
            let today = row
                .try_get::<String, _>("today_usd")
                .map_err(|_| invalid("invalid today cost"))?;
            let total = row
                .try_get::<String, _>("total_usd")
                .map_err(|_| invalid("invalid total cost"))?;
            Ok((
                group_id,
                AccountGroupUsage {
                    today_usd: DecimalAmount::from_str(&today)
                        .map_err(|_| invalid("invalid today cost"))?,
                    total_usd: DecimalAmount::from_str(&total)
                        .map_err(|_| invalid("invalid total cost"))?,
                },
            ))
        })
        .collect()
}

fn group_member(row: &sqlx::postgres::PgRow) -> StoreResult<AccountGroupMember> {
    Ok(AccountGroupMember {
        id: row
            .try_get("id")
            .map_err(|_| invalid("invalid member id"))?,
        name: row
            .try_get("name")
            .map_err(|_| invalid("invalid member name"))?,
        provider_kind: ProviderKind::new(
            row.try_get::<String, _>("provider_kind")
                .map_err(|_| invalid("invalid member provider"))?,
        )
        .map_err(|_| invalid("invalid member provider"))?,
        email: row
            .try_get("email")
            .map_err(|_| invalid("invalid member email"))?,
        enabled: row
            .try_get("enabled")
            .map_err(|_| invalid("invalid member enabled"))?,
    })
}

fn count_value(row: &sqlx::postgres::PgRow, field: &str) -> StoreResult<u64> {
    u64::try_from(
        row.try_get::<i64, _>(field)
            .map_err(|_| invalid("invalid count"))?,
    )
    .map_err(|_| invalid("negative count"))
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
    if name.trim() != name
        || name.is_empty()
        || name.chars().count() > 100
        || name.chars().any(char::is_control)
        || description
            .is_some_and(|value| value.len() > 4096 || value.chars().any(char::is_control))
    {
        return Err(invalid_admin("invalid account group fields"));
    }
    Ok(())
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
        unavailable("write account group")
    }
}

fn not_found(id: &str) -> AdminStoreError {
    admin_store_error(ENTITY, not_found_store(id))
}

fn not_found_store(id: &str) -> StoreError {
    StoreError::NotFound {
        entity: ENTITY,
        id: id.to_owned(),
    }
}

fn conflict(id: &str) -> StoreError {
    StoreError::Conflict {
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
        entity: ENTITY,
        message: message.to_owned(),
    }
}

fn unavailable(message: &'static str) -> StoreError {
    postgres_unavailable(message)
}
