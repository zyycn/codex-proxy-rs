//! 插件下载凭据的加密持久化、读取与引用清理

use gateway_admin::model::audit::MutationAuditOperation;
use gateway_admin::{
    model::{
        MutationContext, Revision,
        plugins::distribution::{SourceAuthentication, SourceCredential, SourceCredentialInfo},
    },
    ports::store::AdminStoreResult,
};
use secrecy::ExposeSecret as _;
use sqlx::{PgPool, Postgres, Row as _, Transaction};

use super::super::{append_admin_audit_event_in_transaction, bump_config_revision_in_transaction};
use super::artifacts::{conflict, not_found, unavailable};
use crate::{admin_revision, admin_store_error, mutation_audit};

pub(super) async fn list(pool: &PgPool) -> AdminStoreResult<Vec<SourceCredentialInfo>> {
    let rows: Vec<sqlx::types::Json<SourceCredentialInfo>> =
        sqlx::query_scalar("select info_json from plugin_source_credentials order by id")
            .fetch_all(pool)
            .await
            .map_err(|_| unavailable())?;
    Ok(rows.into_iter().map(|row| row.0).collect())
}

pub(super) async fn load(pool: &PgPool, id: &str) -> AdminStoreResult<SourceCredential> {
    let id = uuid::Uuid::parse_str(id).map_err(|_| not_found())?;
    let row =
        sqlx::query("select info_json, secret_json from plugin_source_credentials where id=$1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(not_found)?;
    let info: sqlx::types::Json<SourceCredentialInfo> =
        row.try_get("info_json").map_err(|_| unavailable())?;
    let authentication: sqlx::types::Json<SourceAuthentication> =
        row.try_get("secret_json").map_err(|_| unavailable())?;
    Ok(SourceCredential {
        info: info.0,
        authentication: authentication.0,
    })
}

pub(super) async fn save(
    pool: &PgPool,
    credential: SourceCredential,
    context: &MutationContext,
) -> AdminStoreResult<Revision> {
    let id = uuid::Uuid::parse_str(&credential.info.id).map_err(|_| unavailable())?;
    // 只有持久化边界显式展开 secret，不给敏感领域类型实现通用 Serialize
    let secret = match &credential.authentication {
        SourceAuthentication::Github { token } => {
            serde_json::json!({"kind":"github", "token":token.expose_secret()})
        }
        SourceAuthentication::Bearer { token } => {
            serde_json::json!({"kind":"bearer", "token":token.expose_secret()})
        }
        SourceAuthentication::Basic { username, password } => {
            serde_json::json!({"kind":"basic", "username":username, "password":password.expose_secret()})
        }
        SourceAuthentication::Header { name, value } => {
            serde_json::json!({"kind":"header", "name":name, "value":value.expose_secret()})
        }
    };
    let mut tx = pool.begin().await.map_err(|_| unavailable())?;
    let revision = bump_config_revision_in_transaction(&mut tx)
        .await
        .map_err(|e| admin_store_error("plugin", e))?;
    sqlx::query(
        "insert into plugin_source_credentials(id,info_json,secret_json) values ($1,$2,$3)",
    )
    .bind(id)
    .bind(sqlx::types::Json(&credential.info))
    .bind(secret)
    .execute(&mut *tx)
    .await
    .map_err(|_| conflict())?;
    append_admin_audit_event_in_transaction(
        &mut tx,
        mutation_audit(
            context,
            MutationAuditOperation::PluginSourceCredentialCreate,
            &credential.info.id,
            vec!["credential".into()],
        ),
        revision,
    )
    .await
    .map_err(|e| admin_store_error("plugin", e))?;
    tx.commit().await.map_err(|_| unavailable())?;
    admin_revision(revision)
}

/// 只回收本次删除制品使用过且已无引用的凭据，保留其他插件共享的下载认证
pub(super) async fn delete_unused(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[uuid::Uuid],
    context: &MutationContext,
    revision: crate::Revision,
) -> AdminStoreResult<()> {
    let removed: Vec<uuid::Uuid> = sqlx::query_scalar(
        "delete from plugin_source_credentials c where c.id=any($1)
         and not exists (select 1 from plugin_artifact_credentials r where r.credential_id=c.id)
         returning c.id",
    )
    .bind(ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| unavailable())?;
    for id in removed {
        append_admin_audit_event_in_transaction(
            tx,
            mutation_audit(
                context,
                MutationAuditOperation::PluginSourceCredentialDelete,
                &id.to_string(),
                vec!["credential".into()],
            ),
            revision,
        )
        .await
        .map_err(|error| admin_store_error("plugin", error))?;
    }
    Ok(())
}

pub(super) async fn delete(
    pool: &PgPool,
    id: &str,
    context: &MutationContext,
) -> AdminStoreResult<Revision> {
    let key = uuid::Uuid::parse_str(id).map_err(|_| not_found())?;
    let mut tx = pool.begin().await.map_err(|_| unavailable())?;
    let revision = bump_config_revision_in_transaction(&mut tx)
        .await
        .map_err(|e| admin_store_error("plugin", e))?;
    let result = sqlx::query("delete from plugin_source_credentials where id=$1")
        .bind(key)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if e.as_database_error()
                .is_some_and(|e| e.is_foreign_key_violation())
            {
                conflict()
            } else {
                unavailable()
            }
        })?;
    if result.rows_affected() == 0 {
        return Err(not_found());
    }
    append_admin_audit_event_in_transaction(
        &mut tx,
        mutation_audit(
            context,
            MutationAuditOperation::PluginSourceCredentialDelete,
            id,
            vec!["credential".into()],
        ),
        revision,
    )
    .await
    .map_err(|e| admin_store_error("plugin", e))?;
    tx.commit().await.map_err(|_| unavailable())?;
    admin_revision(revision)
}
