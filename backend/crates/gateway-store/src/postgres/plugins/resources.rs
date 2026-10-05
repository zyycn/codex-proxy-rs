//! 插件资源写入在同一事务中验证当前实例、访问域与资源归属

use super::super::{
    account_groups::insert_account_group_in_transaction,
    append_admin_audit_event_in_transaction, bump_config_revision_in_transaction,
    client_keys::{NewClientApiKey, insert_client_api_key_in_transaction},
};
use super::PgPluginStore;
use crate::{admin_revision, admin_store_error, mutation_audit};
use async_trait::async_trait;
use gateway_admin::model::audit::MutationAuditOperation;
use gateway_admin::{
    model::{
        MutationContext,
        account_groups::NewAccountGroup,
        client_keys::NewClientKey,
        plugin_resources::{
            GroupMembersChange, GroupMembersChanged, ManagedResource, PluginResourceOwner,
            ResourceMutation,
        },
    },
    ports::{
        plugin_resources::PluginResourceStore,
        store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
    },
};
use sqlx::{Postgres, Row as _, Transaction};
use std::collections::BTreeSet;

fn unavailable() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        "plugin resource",
        "plugin resource store is unavailable",
    )
}

fn denied() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Conflict,
        "plugin resource",
        "plugin instance or resource authorization changed",
    )
}
fn invalid() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Invalid,
        "plugin resource",
        "invalid plugin resource input",
    )
}

fn validate_resource_key(key: &str) -> AdminStoreResult<()> {
    if key.is_empty()
        || key.len() > 64
        || !key.as_bytes()[0].is_ascii_alphanumeric()
        || !key
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(&c))
    {
        return Err(invalid());
    }
    Ok(())
}

async fn finish<T>(
    mut tx: Transaction<'_, Postgres>,
    context: &MutationContext,
    kind: MutationAuditOperation,
    id: &str,
    changed: bool,
    value: T,
) -> AdminStoreResult<ResourceMutation<T>> {
    let revision = if changed {
        let revision = bump_config_revision_in_transaction(&mut tx)
            .await
            .map_err(|e| admin_store_error("plugin resource", e))?;
        let audit = mutation_audit(context, kind, id, vec!["plugin_owned_resource".to_owned()]);
        append_admin_audit_event_in_transaction(&mut tx, audit, revision)
            .await
            .map_err(|e| admin_store_error("plugin resource", e))?;
        Some(admin_revision(revision)?)
    } else {
        None
    };
    tx.commit().await.map_err(|_| unavailable())?;
    Ok(ResourceMutation { revision, value })
}

fn resource(row: &sqlx::postgres::PgRow) -> AdminStoreResult<ManagedResource> {
    Ok(ManagedResource {
        id: row.try_get("id").map_err(|_| unavailable())?,
        name: row.try_get("name").map_err(|_| unavailable())?,
        enabled: row.try_get("enabled").map_err(|_| unavailable())?,
    })
}

#[async_trait]
impl PluginResourceStore for PgPluginStore {
    async fn ensure_group(
        &self,
        owner: &PluginResourceOwner,
        resource_key: String,
        command: NewAccountGroup,
        context: &MutationContext,
    ) -> AdminStoreResult<ResourceMutation<ManagedResource>> {
        validate_resource_key(&resource_key)?;
        let mut tx = super::begin_plugin_mutation(&self.pool, owner).await?;
        let instance = uuid::Uuid::parse_str(&owner.instance_id).map_err(|_| denied())?;
        if let Some(row) = sqlx::query(
            "select g.id,g.name,g.enabled from plugin_group_resources r
             join account_groups g on g.id=r.group_id
             where r.instance_id=$1 and r.resource_key=$2",
        )
        .bind(instance)
        .bind(&resource_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| unavailable())?
        {
            return finish(
                tx,
                context,
                MutationAuditOperation::AccountGroupPluginReconcile,
                "",
                false,
                resource(&row)?,
            )
            .await;
        }
        insert_account_group_in_transaction(&mut tx, &command)
            .await
            .map_err(|e| admin_store_error("account group", e))?;
        sqlx::query("insert into plugin_group_resources(instance_id,resource_key,group_id) values($1,$2,$3)")
            .bind(instance)
            .bind(resource_key)
            .bind(command.id.as_str())
            .execute(&mut *tx)
            .await.map_err(|_| unavailable())?;
        let value = ManagedResource {
            id: command.id.as_str().to_owned(),
            name: command.name,
            enabled: true,
        };
        finish(
            tx,
            context,
            MutationAuditOperation::AccountGroupPluginReconcile,
            command.id.as_str(),
            true,
            value,
        )
        .await
    }

    async fn ensure_key(
        &self,
        owner: &PluginResourceOwner,
        resource_key: String,
        groups: Vec<String>,
        command: NewClientKey,
        context: &MutationContext,
    ) -> AdminStoreResult<ResourceMutation<ManagedResource>> {
        validate_resource_key(&resource_key)?;
        if groups.is_empty()
            || groups.len() > 64
            || groups.iter().collect::<BTreeSet<_>>().len() != groups.len()
        {
            return Err(invalid());
        }
        for group in &groups {
            validate_resource_key(group)?;
        }
        let mut tx = super::begin_plugin_mutation(&self.pool, owner).await?;
        let instance = uuid::Uuid::parse_str(&owner.instance_id).map_err(|_| denied())?;
        if let Some(row) = sqlx::query(
            "select k.id,k.name,k.enabled from plugin_key_resources r
             join client_api_keys k on k.id=r.key_id
             where r.instance_id=$1 and r.resource_key=$2",
        )
        .bind(instance)
        .bind(&resource_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| unavailable())?
        {
            return finish(
                tx,
                context,
                MutationAuditOperation::ClientApiKeyPluginReconcile,
                "",
                false,
                resource(&row)?,
            )
            .await;
        }
        let group_ids: Vec<String> = sqlx::query_scalar("select group_id from plugin_group_resources where instance_id=$1 and resource_key=any($2::text[])")
            .bind(instance)
            .bind(&groups)
            .fetch_all(&mut *tx)
            .await.map_err(|_| unavailable())?;
        if group_ids.len() != groups.len() {
            return Err(denied());
        }
        let key = NewClientApiKey {
            request_profile_overrides: command.request_profile_overrides,
            id: command.id.as_str().to_owned(),
            name: command.name,
            label: command.label,
            key: command.plaintext,
            group_ids,
            budget: command.budget,
            max_concurrency: command.limits.max_concurrency,
            requests_per_minute: command.limits.requests_per_minute,
        };
        insert_client_api_key_in_transaction(&mut tx, &key)
            .await
            .map_err(|e| admin_store_error("client API key", e))?;
        sqlx::query(
            "insert into plugin_key_resources(instance_id,resource_key,key_id) values($1,$2,$3)",
        )
        .bind(instance)
        .bind(resource_key)
        .bind(&key.id)
        .execute(&mut *tx)
        .await
        .map_err(|_| unavailable())?;
        let value = ManagedResource {
            id: key.id.clone(),
            name: key.name.trim().to_owned(),
            enabled: true,
        };
        finish(
            tx,
            context,
            MutationAuditOperation::ClientApiKeyPluginReconcile,
            &key.id,
            true,
            value,
        )
        .await
    }

    async fn change_members(
        &self,
        owner: &PluginResourceOwner,
        command: GroupMembersChange,
        context: &MutationContext,
    ) -> AdminStoreResult<ResourceMutation<GroupMembersChanged>> {
        validate_resource_key(&command.resource_key)?;
        let add: BTreeSet<_> = command.add.iter().collect();
        let remove: BTreeSet<_> = command.remove.iter().collect();
        if command.add.len() + command.remove.len() > 200
            || !add.is_disjoint(&remove)
            || add.len() != command.add.len()
            || remove.len() != command.remove.len()
        {
            return Err(invalid());
        }
        let mut tx = super::begin_plugin_mutation(&self.pool, owner).await?;
        let instance = uuid::Uuid::parse_str(&owner.instance_id).map_err(|_| denied())?;
        let group: String = sqlx::query_scalar(
            "select group_id from plugin_group_resources where instance_id=$1 and resource_key=$2",
        )
        .bind(instance)
        .bind(command.resource_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(denied)?;
        // 已删除账号自然不再需要加入；外键负责与并发账号删除保持一致
        let added = sqlx::query(
            "insert into account_group_accounts(account_group_id,provider_account_id,created_at)
             select $1,id,now() from provider_accounts where id=any($2::text[])
             on conflict do nothing",
        )
        .bind(&group)
        .bind(command.add)
        .execute(&mut *tx)
        .await
        .map_err(|_| unavailable())?
        .rows_affected();
        let removed = sqlx::query(
            "delete from account_group_accounts
             where account_group_id=$1 and provider_account_id=any($2::text[])",
        )
        .bind(&group)
        .bind(command.remove)
        .execute(&mut *tx)
        .await
        .map_err(|_| unavailable())?
        .rows_affected();
        finish(
            tx,
            context,
            MutationAuditOperation::AccountGroupPluginReconcile,
            &group,
            added + removed > 0,
            GroupMembersChanged { added, removed },
        )
        .await
    }
}
