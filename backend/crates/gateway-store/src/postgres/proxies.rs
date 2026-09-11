//! Named proxies own credentials; accounts retain the resolved URL for provider transports.

use async_trait::async_trait;
use gateway_admin::{
    model::{MutationContext, Revision as AdminRevision, proxies::*},
    ports::{
        proxy::ProxyStore,
        store::{AdminStoreError, AdminStoreResult},
    },
};
use gateway_core::account::OutboundProxy;
use sqlx::{PgPool, Postgres, QueryBuilder, Row as _, Transaction, postgres::PgRow};

use super::{append_admin_audit_event_in_transaction, bump_config_revision_in_transaction};
use crate::{
    ConflictKind, Revision, StoreError, StoreResult, admin_revision, admin_store_error,
    mutation_audit, postgres_unavailable,
};

const ENTITY: &str = "outbound proxy";
const SELECT: &str = "select p.*,
    array(select a.id from provider_accounts a where a.outbound_proxy_id = p.id order by a.id) as account_ids,
    array(select a.name from provider_accounts a where a.outbound_proxy_id = p.id order by a.id) as account_names
    from outbound_proxies p";

#[derive(Clone)]
pub struct PgProxyRepository {
    pool: PgPool,
}

impl PgProxyRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn store_error(error: StoreError) -> AdminStoreError {
    admin_store_error(ENTITY, error)
}
fn unavailable() -> StoreError {
    postgres_unavailable("outbound proxy operation")
}
fn conflict(id: &str) -> StoreError {
    StoreError::Conflict {
        entity: ENTITY,
        id: id.to_owned(),
        kind: ConflictKind::InvalidTransition,
    }
}
fn invalid() -> StoreError {
    StoreError::InvalidData {
        entity: ENTITY,
        message: "invalid proxy record".to_owned(),
    }
}

fn record(row: PgRow) -> StoreResult<ProxyRecord> {
    let success: Option<bool> = row.try_get("last_test_success").map_err(|_| invalid())?;
    let ip: Option<String> = row.try_get("last_test_ip").map_err(|_| invalid())?;
    let latency: Option<i64> = row.try_get("last_test_latency_ms").map_err(|_| invalid())?;
    let ids: Vec<String> = row.try_get("account_ids").map_err(|_| invalid())?;
    let names: Vec<String> = row.try_get("account_names").map_err(|_| invalid())?;
    Ok(ProxyRecord {
        id: row.try_get("id").map_err(|_| invalid())?,
        name: row.try_get("name").map_err(|_| invalid())?,
        proxy: OutboundProxy::parse(
            &row.try_get::<String, _>("proxy_url")
                .map_err(|_| invalid())?,
        )
        .map_err(|_| invalid())?,
        revision: AdminRevision::new(
            u64::try_from(row.try_get::<i64, _>("revision").map_err(|_| invalid())?)
                .map_err(|_| invalid())?,
        )
        .map_err(|_| invalid())?,
        accounts: ids
            .into_iter()
            .zip(names)
            .map(|(id, name)| ProxyAccountRef { id, name })
            .collect(),
        last_test_at: row.try_get("last_test_at").map_err(|_| invalid())?,
        last_test: success
            .map(|success| -> StoreResult<_> {
                Ok(ProxyTestResult {
                    success,
                    latency_ms: u64::try_from(latency.ok_or_else(invalid)?)
                        .map_err(|_| invalid())?,
                    exit_ip: ip.map(|ip| ip.parse().map_err(|_| invalid())).transpose()?,
                    message: row
                        .try_get::<Option<String>, _>("last_test_message")
                        .map_err(|_| invalid())?
                        .unwrap_or_default(),
                })
            })
            .transpose()?,
        created_at: row.try_get("created_at").map_err(|_| invalid())?,
        updated_at: row.try_get("updated_at").map_err(|_| invalid())?,
    })
}

async fn lock_url(
    transaction: &mut Transaction<'_, Postgres>,
    proxy: &OutboundProxy,
) -> StoreResult<()> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 739218))")
        .bind(proxy.expose_url())
        .execute(&mut **transaction)
        .await
        .map_err(|_| unavailable())?;
    Ok(())
}

/// Imports and legacy URL writes join the catalog in the same account transaction.
pub(crate) async fn ensure_proxy_for_url(
    transaction: &mut Transaction<'_, Postgres>,
    proxy: &OutboundProxy,
    name: Option<&str>,
) -> StoreResult<(String, bool)> {
    lock_url(transaction, proxy).await?;
    if let Some(id) = sqlx::query_scalar::<_, String>("select id from outbound_proxies where proxy_url = $1 order by created_at, id limit 1 for share")
        .bind(proxy.expose_url()).fetch_optional(&mut **transaction).await.map_err(|_| unavailable())? {
        return Ok((id, false));
    }
    let id = format!("proxy_{}", uuid::Uuid::now_v7().simple());
    let generated_name = format!("Imported proxy {}", &id[id.len() - 8..]);
    sqlx::query("insert into outbound_proxies (id, name, proxy_url) values ($1, $2, $3)")
        .bind(&id)
        .bind(name.unwrap_or(&generated_name))
        .bind(proxy.expose_url())
        .execute(&mut **transaction)
        .await
        .map_err(|_| unavailable())?;
    Ok((id, true))
}

pub(crate) async fn resolve_proxy_selection(
    transaction: &mut Transaction<'_, Postgres>,
    selection: &AccountProxySelection,
) -> StoreResult<(Option<String>, Option<OutboundProxy>)> {
    match selection {
        AccountProxySelection::Direct => Ok((None, None)),
        AccountProxySelection::Url(proxy) => {
            let (id, _) = ensure_proxy_for_url(transaction, proxy, None).await?;
            Ok((Some(id), Some(proxy.clone())))
        }
        AccountProxySelection::Saved(id) => {
            let (value, tested) = sqlx::query_as::<_, (String, Option<bool>)>(
                "select proxy_url, last_test_success from outbound_proxies where id = $1 for share",
            )
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(|| StoreError::NotFound {
                entity: ENTITY,
                id: id.clone(),
            })?;
            if tested != Some(true) {
                return Err(conflict(id));
            }
            Ok((
                Some(id.clone()),
                Some(OutboundProxy::parse(&value).map_err(|_| invalid())?),
            ))
        }
    }
}

async fn audit(
    transaction: &mut Transaction<'_, Postgres>,
    context: &MutationContext,
    action: &str,
    id: &str,
    fields: &[&str],
    revision: Revision,
) -> StoreResult<()> {
    append_admin_audit_event_in_transaction(
        transaction,
        mutation_audit(
            context,
            action,
            "outbound_proxy",
            id,
            fields.iter().map(|value| (*value).to_owned()).collect(),
        ),
        revision,
    )
    .await
}

#[async_trait]
impl ProxyStore for PgProxyRepository {
    async fn list(&self, query: ProxyListQuery) -> AdminStoreResult<ProxyPage> {
        if query.page == 0 {
            return Err(store_error(invalid()));
        }
        let total: i64 = sqlx::query_scalar(
            "select count(*) from outbound_proxies where strpos(lower(name), lower($1)) > 0",
        )
        .bind(&query.search)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| store_error(unavailable()))?;
        let mut builder = QueryBuilder::<Postgres>::new(SELECT);
        builder
            .push(" where strpos(lower(p.name), lower(")
            .push_bind(&query.search)
            .push(")) > 0 order by p.created_at desc, p.id limit ")
            .push_bind(i64::from(query.page_size.get()))
            .push(" offset ")
            .push_bind(i64::from(query.page - 1) * i64::from(query.page_size.get()));
        let items = builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|_| store_error(unavailable()))?
            .into_iter()
            .map(record)
            .collect::<StoreResult<Vec<_>>>()
            .map_err(store_error)?;
        Ok(ProxyPage {
            items,
            total: u64::try_from(total).map_err(|_| store_error(invalid()))?,
            page: query.page,
            page_size: query.page_size.get(),
        })
    }

    async fn get(&self, id: &str) -> AdminStoreResult<ProxyRecord> {
        let mut builder = QueryBuilder::<Postgres>::new(SELECT);
        builder.push(" where p.id = ").push_bind(id);
        let row = builder
            .build()
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| store_error(unavailable()))?
            .ok_or_else(|| {
                store_error(StoreError::NotFound {
                    entity: ENTITY,
                    id: id.to_owned(),
                })
            })?;
        record(row).map_err(store_error)
    }

    async fn create(
        &self,
        command: NewProxy,
        context: &MutationContext,
    ) -> AdminStoreResult<ProxyMutation> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| store_error(unavailable()))?;
        let revision = bump_config_revision_in_transaction(&mut transaction)
            .await
            .map_err(store_error)?;
        let (id, created) =
            ensure_proxy_for_url(&mut transaction, &command.proxy, Some(&command.name))
                .await
                .map_err(store_error)?;
        if !created {
            return Err(store_error(conflict(&id)));
        }
        audit(
            &mut transaction,
            context,
            "create",
            &id,
            &["name", "proxy_url"],
            revision,
        )
        .await
        .map_err(store_error)?;
        transaction
            .commit()
            .await
            .map_err(|_| store_error(unavailable()))?;
        Ok(ProxyMutation {
            config_revision: admin_revision(revision)?,
            record: self.get(&id).await?,
        })
    }

    async fn update(
        &self,
        command: UpdateProxy,
        context: &MutationContext,
    ) -> AdminStoreResult<ProxyMutation> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| store_error(unavailable()))?;
        let revision = bump_config_revision_in_transaction(&mut transaction)
            .await
            .map_err(store_error)?;
        if let Some(proxy) = &command.proxy {
            lock_url(&mut transaction, proxy)
                .await
                .map_err(store_error)?;
            let duplicate: bool = sqlx::query_scalar(
                "select exists(select 1 from outbound_proxies where proxy_url = $1 and id <> $2)",
            )
            .bind(proxy.expose_url())
            .bind(&command.id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| store_error(unavailable()))?;
            if duplicate {
                return Err(store_error(conflict(&command.id)));
            }
        }
        let changed = sqlx::query(
            "update outbound_proxies set name = $3, proxy_url = coalesce($4, proxy_url), revision = revision + 1, updated_at = now(),
             last_test_at = case when $4 is not null and $4 <> proxy_url then null else last_test_at end,
             last_test_success = case when $4 is not null and $4 <> proxy_url then null else last_test_success end,
             last_test_latency_ms = case when $4 is not null and $4 <> proxy_url then null else last_test_latency_ms end,
             last_test_ip = case when $4 is not null and $4 <> proxy_url then null else last_test_ip end,
             last_test_message = case when $4 is not null and $4 <> proxy_url then null else last_test_message end
             where id = $1 and revision = $2")
            .bind(&command.id).bind(i64::try_from(command.revision.get()).map_err(|_| store_error(invalid()))?)
            .bind(&command.name).bind(command.proxy.as_ref().map(OutboundProxy::expose_url))
            .execute(&mut *transaction).await.map_err(|_| store_error(unavailable()))?;
        if changed.rows_affected() != 1 {
            return Err(store_error(conflict(&command.id)));
        }
        sqlx::query("update provider_accounts a set outbound_proxy_url = p.proxy_url, updated_at = greatest(now(), a.updated_at) from outbound_proxies p where p.id = $1 and a.outbound_proxy_id = p.id and a.outbound_proxy_url is distinct from p.proxy_url")
            .bind(&command.id).execute(&mut *transaction).await.map_err(|_| store_error(unavailable()))?;
        audit(
            &mut transaction,
            context,
            "update",
            &command.id,
            &["name", "proxy_url"],
            revision,
        )
        .await
        .map_err(store_error)?;
        transaction
            .commit()
            .await
            .map_err(|_| store_error(unavailable()))?;
        Ok(ProxyMutation {
            config_revision: admin_revision(revision)?,
            record: self.get(&command.id).await?,
        })
    }

    async fn delete(
        &self,
        id: &str,
        revision: AdminRevision,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminRevision> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| store_error(unavailable()))?;
        let config_revision = bump_config_revision_in_transaction(&mut transaction)
            .await
            .map_err(store_error)?;
        let result = sqlx::query("delete from outbound_proxies where id = $1 and revision = $2")
            .bind(id)
            .bind(i64::try_from(revision.get()).map_err(|_| store_error(invalid()))?)
            .execute(&mut *transaction)
            .await
            .map_err(|error| {
                // PostgreSQL 18 distinguishes RESTRICT from other foreign-key violations.
                if error.as_database_error().is_some_and(|error| {
                    error.is_foreign_key_violation() || error.code().as_deref() == Some("23001")
                }) {
                    store_error(conflict(id))
                } else {
                    store_error(unavailable())
                }
            })?;
        if result.rows_affected() != 1 {
            return Err(store_error(conflict(id)));
        }
        audit(
            &mut transaction,
            context,
            "delete",
            id,
            &[],
            config_revision,
        )
        .await
        .map_err(store_error)?;
        transaction
            .commit()
            .await
            .map_err(|_| store_error(unavailable()))?;
        admin_revision(config_revision)
    }

    async fn record_test(
        &self,
        id: &str,
        revision: AdminRevision,
        result: ProxyTestResult,
        context: &MutationContext,
    ) -> AdminStoreResult<ProxyRecord> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| store_error(unavailable()))?;
        let updated = sqlx::query("update outbound_proxies set last_test_at = now(), last_test_success = $3, last_test_latency_ms = $4, last_test_ip = $5, last_test_message = $6 where id = $1 and revision = $2")
            .bind(id).bind(i64::try_from(revision.get()).map_err(|_| store_error(invalid()))?)
            .bind(result.success).bind(i64::try_from(result.latency_ms).map_err(|_| store_error(invalid()))?)
            .bind(result.exit_ip.map(|ip| ip.to_string())).bind(result.message)
            .execute(&mut *transaction).await.map_err(|_| store_error(unavailable()))?;
        if updated.rows_affected() != 1 {
            return Err(store_error(conflict(id)));
        }
        let current: i64 =
            sqlx::query_scalar("select config_revision from runtime_settings where id = 1")
                .fetch_one(&mut *transaction)
                .await
                .map_err(|_| store_error(unavailable()))?;
        let current = Revision::new(u64::try_from(current).map_err(|_| store_error(invalid()))?)
            .map_err(store_error)?;
        audit(
            &mut transaction,
            context,
            "test",
            id,
            &["last_test"],
            current,
        )
        .await
        .map_err(store_error)?;
        transaction
            .commit()
            .await
            .map_err(|_| store_error(unavailable()))?;
        self.get(id).await
    }
}
