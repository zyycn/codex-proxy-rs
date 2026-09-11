//! PostgreSQL owner for the outbound proxy pool.

use async_trait::async_trait;
use futures::future::BoxFuture;
use gateway_admin::{
    model::{
        MutationContext,
        proxies::{
            DeleteOutboundProxy, NewOutboundProxy, OutboundProxyId, OutboundProxyListQuery,
            OutboundProxyMutation, OutboundProxyPage, OutboundProxyRecord, RevealedOutboundProxy,
            UpdateOutboundProxy,
        },
    },
    ports::store::{AdminStoreError, AdminStoreResult, OutboundProxyStore},
};
use gateway_core::account::OutboundProxy;
use sqlx::{PgPool, Postgres, QueryBuilder, Row as _, Transaction};

use crate::{
    ConflictKind, StoreError, StoreResult, admin_revision, admin_store_error, mutation_audit,
    postgres_unavailable,
};

use super::{
    PgRuntimeSettingsRepository, RuntimeSettingsRepository,
    append_admin_audit_event_in_transaction, bump_config_revision_in_transaction,
};

const ENTITY: &str = "outbound proxy";

/// Outbound proxy pool store with transactional revision and audit ownership.
#[derive(Clone)]
pub struct PgOutboundProxyRepository {
    pool: PgPool,
}

impl PgOutboundProxyRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn current_revision(&self) -> AdminStoreResult<gateway_admin::model::Revision> {
        RuntimeSettingsRepository::load_runtime_settings(&PgRuntimeSettingsRepository::new(
            self.pool.clone(),
        ))
        .await
        .map_err(|error| admin_store_error(ENTITY, error))
        .and_then(|settings| admin_revision(settings.config_revision))
    }

    async fn required_record(&self, id: &OutboundProxyId) -> AdminStoreResult<OutboundProxyRecord> {
        load_record(&self.pool, id.as_str())
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?
            .ok_or_else(|| not_found(id.as_str()))
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
                admin_store_error(ENTITY, unavailable("begin outbound proxy mutation"))
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
                    admin_store_error(ENTITY, unavailable("commit outbound proxy mutation"))
                })?;
                admin_revision(revision)
            }
            Err(error) => {
                transaction.rollback().await.map_err(|_| {
                    admin_store_error(ENTITY, unavailable("rollback outbound proxy mutation"))
                })?;
                Err(admin_store_error(ENTITY, error))
            }
        }
    }
}

#[async_trait]
impl OutboundProxyStore for PgOutboundProxyRepository {
    async fn list_outbound_proxies(
        &self,
        query: OutboundProxyListQuery,
    ) -> AdminStoreResult<OutboundProxyPage> {
        validate_page_query(&query)?;
        let total = count_proxies(&self.pool, &query)
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        let offset = u64::from(query.page.saturating_sub(1)) * u64::from(query.page_size.get());
        let mut statement = proxy_select();
        push_proxy_filter(&mut statement, &query);
        statement.push(" order by p.created_at desc, p.id desc limit ");
        statement.push_bind(i64::from(query.page_size.get()));
        statement.push(" offset ");
        statement.push_bind(i64::try_from(offset).map_err(|_| invalid_admin("page is too large"))?);
        let rows = statement
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|_| admin_store_error(ENTITY, unavailable("list outbound proxies")))?;
        let items = rows
            .iter()
            .map(proxy_record)
            .collect::<StoreResult<Vec<_>>>()
            .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok(OutboundProxyPage {
            config_revision: self.current_revision().await?,
            items,
            total,
            page: query.page,
            page_size: query.page_size.get(),
        })
    }

    async fn load_outbound_proxy(
        &self,
        id: &OutboundProxyId,
    ) -> AdminStoreResult<Option<RevealedOutboundProxy>> {
        let row = sqlx::query("select id, url from outbound_proxies where id = $1")
            .bind(id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| admin_store_error(ENTITY, unavailable("load outbound proxy")))?;
        row.map(|row| {
            Ok(RevealedOutboundProxy {
                id: proxy_id(&row)?,
                proxy: proxy_url(&row)?,
            })
        })
        .transpose()
        .map_err(|error| admin_store_error(ENTITY, error))
    }

    async fn create_outbound_proxy(
        &self,
        command: NewOutboundProxy,
        context: &MutationContext,
    ) -> AdminStoreResult<OutboundProxyMutation> {
        validate_proxy_name(&command.name)?;
        let id = command.id.clone();
        let audit = mutation_audit(
            context,
            "create",
            "outbound_proxy",
            id.as_str(),
            vec!["name".to_owned(), "url".to_owned()],
        );
        let revision = self
            .mutate(audit, |transaction| {
                Box::pin(async move {
                    sqlx::query(
                        "insert into outbound_proxies (id, name, url, created_at, updated_at)
                         values ($1, $2, $3, now(), now())",
                    )
                    .bind(command.id.as_str())
                    .bind(command.name)
                    .bind(command.proxy.expose_url())
                    .execute(&mut **transaction)
                    .await
                    .map_err(|error| map_proxy_write_error(error, command.id.as_str()))?;
                    Ok(())
                })
            })
            .await?;
        Ok(OutboundProxyMutation {
            config_revision: revision,
            id: id.clone(),
            record: Some(self.required_record(&id).await?),
        })
    }

    async fn update_outbound_proxy(
        &self,
        command: UpdateOutboundProxy,
        context: &MutationContext,
    ) -> AdminStoreResult<OutboundProxyMutation> {
        validate_proxy_name(&command.name)?;
        let id = command.id.clone();
        let mut changed_fields = vec!["name".to_owned()];
        if command.proxy.is_some() {
            changed_fields.push("url".to_owned());
        }
        let audit = mutation_audit(
            context,
            "update",
            "outbound_proxy",
            id.as_str(),
            changed_fields,
        );
        let revision = self
            .mutate(audit, |transaction| {
                Box::pin(async move {
                    let result = sqlx::query(
                        "update outbound_proxies
                            set name = $2, url = coalesce($3, url), updated_at = now()
                          where id = $1",
                    )
                    .bind(command.id.as_str())
                    .bind(command.name)
                    .bind(command.proxy.as_ref().map(OutboundProxy::expose_url))
                    .execute(&mut **transaction)
                    .await
                    .map_err(|error| map_proxy_write_error(error, command.id.as_str()))?;
                    require_one(result.rows_affected(), command.id.as_str())
                })
            })
            .await?;
        Ok(OutboundProxyMutation {
            config_revision: revision,
            id: id.clone(),
            record: Some(self.required_record(&id).await?),
        })
    }

    async fn delete_outbound_proxy(
        &self,
        command: DeleteOutboundProxy,
        context: &MutationContext,
    ) -> AdminStoreResult<OutboundProxyMutation> {
        let id = command.id.clone();
        let audit = mutation_audit(context, "delete", "outbound_proxy", id.as_str(), Vec::new());
        let revision = self
            .mutate(audit, |transaction| {
                Box::pin(async move {
                    let result = sqlx::query("delete from outbound_proxies where id = $1")
                        .bind(command.id.as_str())
                        .execute(&mut **transaction)
                        .await
                        .map_err(|_| unavailable("delete outbound proxy"))?;
                    require_one(result.rows_affected(), command.id.as_str())
                })
            })
            .await?;
        Ok(OutboundProxyMutation {
            config_revision: revision,
            id,
            record: None,
        })
    }
}

fn proxy_select() -> QueryBuilder<Postgres> {
    QueryBuilder::new(
        "select p.id, p.name, p.url, p.created_at, p.updated_at,
                coalesce(accounts.account_count, 0)::bigint as account_count
         from outbound_proxies p
         left join lateral (
           select count(*)::bigint as account_count
           from provider_accounts a
           where a.outbound_proxy_url = p.url
         ) accounts on true
         where true",
    )
}

fn push_proxy_filter(statement: &mut QueryBuilder<Postgres>, query: &OutboundProxyListQuery) {
    if let Some(search) = &query.search {
        statement.push(" and lower(p.name) like ");
        statement.push_bind(format!("%{}%", search.to_lowercase()));
    }
}

async fn count_proxies(pool: &PgPool, query: &OutboundProxyListQuery) -> StoreResult<u64> {
    let mut statement =
        QueryBuilder::<Postgres>::new("select count(*)::bigint from outbound_proxies p where true");
    if let Some(search) = &query.search {
        statement.push(" and lower(p.name) like ");
        statement.push_bind(format!("%{}%", search.to_lowercase()));
    }
    let count = statement
        .build_query_scalar::<i64>()
        .fetch_one(pool)
        .await
        .map_err(|_| unavailable("count outbound proxies"))?;
    u64::try_from(count).map_err(|_| invalid("negative outbound proxy count"))
}

async fn load_record(pool: &PgPool, id: &str) -> StoreResult<Option<OutboundProxyRecord>> {
    let mut statement = proxy_select();
    statement.push(" and p.id = ");
    statement.push_bind(id.to_owned());
    statement
        .build()
        .fetch_optional(pool)
        .await
        .map_err(|_| unavailable("load outbound proxy"))?
        .as_ref()
        .map(proxy_record)
        .transpose()
}

fn proxy_record(row: &sqlx::postgres::PgRow) -> StoreResult<OutboundProxyRecord> {
    Ok(OutboundProxyRecord {
        id: proxy_id(row)?,
        name: row.try_get("name").map_err(|_| invalid("invalid name"))?,
        proxy: proxy_url(row)?,
        account_count: u64::try_from(
            row.try_get::<i64, _>("account_count")
                .map_err(|_| invalid("invalid account count"))?,
        )
        .map_err(|_| invalid("negative account count"))?,
        created_at: row
            .try_get("created_at")
            .map_err(|_| invalid("invalid created_at"))?,
        updated_at: row
            .try_get("updated_at")
            .map_err(|_| invalid("invalid updated_at"))?,
    })
}

fn proxy_id(row: &sqlx::postgres::PgRow) -> StoreResult<OutboundProxyId> {
    OutboundProxyId::new(
        row.try_get::<String, _>("id")
            .map_err(|_| invalid("invalid id"))?,
    )
    .map_err(|_| invalid("invalid id"))
}

fn proxy_url(row: &sqlx::postgres::PgRow) -> StoreResult<OutboundProxy> {
    OutboundProxy::parse(
        &row.try_get::<String, _>("url")
            .map_err(|_| invalid("invalid url"))?,
    )
    .map_err(|_| invalid("invalid url"))
}

fn validate_page_query(query: &OutboundProxyListQuery) -> AdminStoreResult<()> {
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

fn validate_proxy_name(name: &str) -> AdminStoreResult<()> {
    if name.trim() != name
        || name.is_empty()
        || name.chars().count() > 100
        || name.chars().any(char::is_control)
    {
        return Err(invalid_admin("invalid outbound proxy name"));
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

fn map_proxy_write_error(error: sqlx::Error, id: &str) -> StoreError {
    if error
        .as_database_error()
        .is_some_and(|database| database.is_unique_violation())
    {
        conflict(id)
    } else {
        unavailable("write outbound proxy")
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
