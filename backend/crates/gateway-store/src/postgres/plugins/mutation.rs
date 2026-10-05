//! 插件写入与实例发布共用控制面锁，避免一个事务混用不同配置版本

use gateway_admin::{
    model::plugin_resources::PluginResourceOwner,
    ports::store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
};
use sqlx::{PgPool, Postgres, Transaction};

fn mutation_unavailable() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        "plugin mutation",
        "plugin mutation store is unavailable",
    )
}

fn mutation_denied() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Conflict,
        "plugin mutation",
        "plugin instance revision changed",
    )
}

pub(in crate::postgres) async fn begin_plugin_mutation<'a>(
    pool: &'a PgPool,
    owner: &PluginResourceOwner,
) -> AdminStoreResult<Transaction<'a, Postgres>> {
    let mut tx = pool.begin().await.map_err(|_| mutation_unavailable())?;
    // 与全部管理写入保持相同锁顺序；无变化的对账只锁定，不递增 revision
    sqlx::query("select config_revision from runtime_settings where id=1 for update")
        .execute(&mut *tx)
        .await
        .map_err(|_| mutation_unavailable())?;
    let id = uuid::Uuid::parse_str(&owner.instance_id).map_err(|_| mutation_denied())?;
    let revision = i64::try_from(owner.revision.get()).map_err(|_| mutation_denied())?;
    let allowed: bool = sqlx::query_scalar(
        "select exists(
                select 1 from plugin_instances i
                join plugin_artifacts a on a.sha256=i.artifact_sha256
                where i.id=$1 and i.enabled and i.revision=$2 and i.artifact_sha256=$3
                  and a.accepted_at is not null
            )",
    )
    .bind(id)
    .bind(revision)
    .bind(&owner.artifact_sha256)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| mutation_unavailable())?;
    if !allowed {
        return Err(mutation_denied());
    }
    Ok(tx)
}
