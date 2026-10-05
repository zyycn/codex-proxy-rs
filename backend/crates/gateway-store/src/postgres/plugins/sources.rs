//! 插件更新来源的持久化、制品绑定与出站代理引用校验

use gateway_admin::model::audit::MutationAuditOperation;
use gateway_admin::{
    model::{
        MutationContext, Revision,
        plugins::{
            PluginSourceEgress,
            distribution::{PluginSourceBinding, PluginUpdatePolicy, PluginUpdateSource},
        },
    },
    ports::store::AdminStoreResult,
};
use sqlx::{PgPool, Postgres, Transaction};

use super::super::{append_admin_audit_event_in_transaction, bump_config_revision_in_transaction};
use super::artifacts::{conflict, not_found, unavailable};
use crate::{admin_revision, admin_store_error, mutation_audit};

#[derive(sqlx::FromRow)]
struct PluginSourceRow {
    plugin_id: String,
    source_json: sqlx::types::Json<PluginUpdateSource>,
    policy_json: sqlx::types::Json<PluginUpdatePolicy>,
    outbound_proxy_id: Option<String>,
}

pub(super) async fn list(pool: &PgPool) -> AdminStoreResult<Vec<PluginSourceBinding>> {
    let rows: Vec<PluginSourceRow> = sqlx::query_as(
        "select plugin_id,source_json,policy_json,outbound_proxy_id from plugin_update_sources order by plugin_id",
    )
    .fetch_all(pool)
    .await
    .map_err(|_| unavailable())?;
    Ok(rows
        .into_iter()
        .map(|row| PluginSourceBinding {
            plugin_id: row.plugin_id,
            source: row.source_json.0,
            policy: row.policy_json.0,
            outbound_proxy_id: row.outbound_proxy_id,
        })
        .collect())
}

pub(super) async fn change(
    pool: &PgPool,
    binding: PluginSourceBinding,
    context: &MutationContext,
) -> AdminStoreResult<Revision> {
    let mut tx = pool.begin().await.map_err(|_| unavailable())?;
    let revision = bump_config_revision_in_transaction(&mut tx)
        .await
        .map_err(|e| admin_store_error("plugin", e))?;
    if let Some(id) = binding.outbound_proxy_id.as_deref() {
        lock_proxy(&mut tx, id, None).await?;
    }
    let result = sqlx::query(
        "update plugin_update_sources set source_json=$2,policy_json=$3,outbound_proxy_id=$4 where plugin_id=$1",
    )
    .bind(&binding.plugin_id)
    .bind(sqlx::types::Json(binding.source))
    .bind(sqlx::types::Json(binding.policy))
    .bind(binding.outbound_proxy_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| unavailable())?;
    if result.rows_affected() == 0 {
        return Err(not_found());
    }
    append_admin_audit_event_in_transaction(
        &mut tx,
        mutation_audit(
            context,
            MutationAuditOperation::PluginSourceChangeSource,
            &binding.plugin_id,
            vec!["source".into(), "policy".into(), "outbound_proxy".into()],
        ),
        revision,
    )
    .await
    .map_err(|e| admin_store_error("plugin", e))?;
    tx.commit().await.map_err(|_| unavailable())?;
    admin_revision(revision)
}

/// 调用方先锁全局 revision，来源确认与制品写入共用同一个事务
pub(super) async fn bind(
    tx: &mut Transaction<'_, Postgres>,
    plugin_id: &str,
    source: PluginUpdateSource,
    outbound_proxy: Option<&PluginSourceEgress>,
) -> AdminStoreResult<()> {
    if let Some(proxy) = outbound_proxy {
        lock_proxy(tx, &proxy.id, Some(proxy.revision)).await?;
    }
    let outbound_proxy_id = outbound_proxy.map(|proxy| proxy.id.as_str());
    sqlx::query("insert into plugin_update_sources(plugin_id,source_json,outbound_proxy_id) values ($1,$2,$3) on conflict do nothing")
        .bind(plugin_id).bind(sqlx::types::Json(&source)).bind(outbound_proxy_id).execute(&mut **tx).await.map_err(|_| unavailable())?;
    let stored: (sqlx::types::Json<PluginUpdateSource>, Option<String>) = sqlx::query_as(
        "select source_json,outbound_proxy_id from plugin_update_sources where plugin_id=$1",
    )
    .bind(plugin_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| unavailable())?;
    if stored.0.0 != source || stored.1.as_deref() != outbound_proxy_id {
        return Err(conflict());
    }
    Ok(())
}

/// 与制品删除共用全局 revision 锁，最后一个版本删除后不保留重装限制
pub(super) async fn delete_if_unused(
    tx: &mut Transaction<'_, Postgres>,
    plugin_id: &str,
    context: &MutationContext,
    revision: crate::Revision,
) -> AdminStoreResult<()> {
    let removed = sqlx::query(
        "delete from plugin_update_sources where plugin_id=$1
         and not exists (select 1 from plugin_artifacts where plugin_id=$1)",
    )
    .bind(plugin_id)
    .execute(&mut **tx)
    .await
    .map_err(|_| unavailable())?;
    if removed.rows_affected() > 0 {
        append_admin_audit_event_in_transaction(
            tx,
            mutation_audit(
                context,
                MutationAuditOperation::PluginSourceDelete,
                plugin_id,
                vec!["source".into(), "policy".into(), "outbound_proxy".into()],
            ),
            revision,
        )
        .await
        .map_err(|error| admin_store_error("plugin", error))?;
    }
    Ok(())
}

async fn lock_proxy(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    expected_revision: Option<u64>,
) -> AdminStoreResult<()> {
    let revision: Option<i64> =
        sqlx::query_scalar("select revision from outbound_proxies where id=$1 for share")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| unavailable())?;
    let Some(revision) = revision else {
        return Err(if expected_revision.is_some() {
            conflict()
        } else {
            not_found()
        });
    };
    if expected_revision.is_some_and(|expected| u64::try_from(revision).ok() != Some(expected)) {
        return Err(conflict());
    }
    Ok(())
}
