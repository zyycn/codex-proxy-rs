//! Account group management use cases.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::SystemTime,
};

use async_trait::async_trait;
use gateway_core::{
    account::{AccountStatus, resolve_account_status},
    routing::AccountGroupId,
    runtime::SnapshotControl,
};
use uuid::Uuid;

use crate::{
    model::{
        AdminError, MutationContext,
        account_groups::{
            AccountGroupAccountSummary, AccountGroupCapacity, AccountGroupListQuery,
            AccountGroupMemberFact, AccountGroupMutation, AccountGroupPage, AccountGroupRecord,
            CreateAccountGroup, DeleteAccountGroup, NewAccountGroup, SetAccountGroupEnabled,
            UpdateAccountGroup,
        },
    },
    ports::store::{AccountGroupStore, AccountRuntimeStore},
};

use super::{map_store_error, publish_committed};

/// API-facing account group management service.
#[async_trait]
pub trait AccountGroupService: Send + Sync {
    async fn list(&self, query: AccountGroupListQuery) -> Result<AccountGroupPage, AdminError>;
    async fn create(
        &self,
        context: &MutationContext,
        command: CreateAccountGroup,
    ) -> Result<AccountGroupMutation, AdminError>;
    async fn update(
        &self,
        context: &MutationContext,
        command: UpdateAccountGroup,
    ) -> Result<AccountGroupMutation, AdminError>;
    async fn set_enabled(
        &self,
        context: &MutationContext,
        command: SetAccountGroupEnabled,
    ) -> Result<AccountGroupMutation, AdminError>;
    async fn delete(
        &self,
        context: &MutationContext,
        command: DeleteAccountGroup,
    ) -> Result<AccountGroupMutation, AdminError>;
}

pub(crate) struct DefaultAccountGroupService {
    store: Arc<dyn AccountGroupStore>,
    runtime: Arc<dyn AccountRuntimeStore>,
    snapshot: Arc<dyn SnapshotControl>,
}

impl DefaultAccountGroupService {
    #[must_use]
    pub(crate) fn new(
        store: Arc<dyn AccountGroupStore>,
        runtime: Arc<dyn AccountRuntimeStore>,
        snapshot: Arc<dyn SnapshotControl>,
    ) -> Self {
        Self {
            store,
            runtime,
            snapshot,
        }
    }

    async fn enrich_records(&self, records: &mut [AccountGroupRecord]) -> Result<(), AdminError> {
        if records.is_empty() {
            return Ok(());
        }
        let group_ids = records
            .iter()
            .map(|record| record.id.clone())
            .collect::<Vec<_>>();
        let members = self
            .store
            .load_account_group_members(&group_ids)
            .await
            .map_err(|error| map_store_error(error, "account group members"))?;
        let account_ids = members
            .iter()
            .map(|member| member.account_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let runtime = match self.runtime.account_runtime(&account_ids).await {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::warn!(error = %error, "account group runtime projection is unavailable");
                Default::default()
            }
        };
        project_group_runtime(records, &members, &runtime);
        Ok(())
    }

    async fn publish(
        &self,
        result: Result<AccountGroupMutation, crate::ports::store::AdminStoreError>,
    ) -> Result<AccountGroupMutation, AdminError> {
        let mut result = result.map_err(|error| map_store_error(error, "account group"))?;
        if let Some(record) = result.record.as_mut() {
            self.enrich_records(std::slice::from_mut(record)).await?;
        }
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }
}

#[async_trait]
impl AccountGroupService for DefaultAccountGroupService {
    async fn list(&self, query: AccountGroupListQuery) -> Result<AccountGroupPage, AdminError> {
        let mut page = self
            .store
            .list_account_groups(query)
            .await
            .map_err(|error| map_store_error(error, "account group"))?;
        self.enrich_records(&mut page.items).await?;
        Ok(page)
    }

    async fn create(
        &self,
        context: &MutationContext,
        command: CreateAccountGroup,
    ) -> Result<AccountGroupMutation, AdminError> {
        let id = AccountGroupId::new(format!("grp_{}", Uuid::now_v7().simple()))
            .map_err(|_| AdminError::internal("创建账号组 ID 失败"))?;
        self.publish(
            self.store
                .create_account_group(
                    NewAccountGroup {
                        id,
                        name: command.name,
                        description: command.description,
                        color: command.color,
                    },
                    context,
                )
                .await,
        )
        .await
    }

    async fn update(
        &self,
        context: &MutationContext,
        command: UpdateAccountGroup,
    ) -> Result<AccountGroupMutation, AdminError> {
        self.publish(self.store.update_account_group(command, context).await)
            .await
    }

    async fn set_enabled(
        &self,
        context: &MutationContext,
        command: SetAccountGroupEnabled,
    ) -> Result<AccountGroupMutation, AdminError> {
        self.publish(self.store.set_account_group_enabled(command, context).await)
            .await
    }

    async fn delete(
        &self,
        context: &MutationContext,
        command: DeleteAccountGroup,
    ) -> Result<AccountGroupMutation, AdminError> {
        self.publish(self.store.delete_account_group(command, context).await)
            .await
    }
}

fn project_group_runtime(
    records: &mut [AccountGroupRecord],
    members: &[AccountGroupMemberFact],
    runtime: &crate::model::accounts::AccountRuntimeSnapshot,
) {
    let now = SystemTime::now();
    let mut status_by_account = BTreeMap::new();
    let mut slots_by_account = BTreeMap::new();
    let mut accounts_by_group = BTreeMap::<&str, Vec<&str>>::new();
    for member in members {
        let mut status = member.status.clone();
        status.rate_limited_until = runtime
            .rate_limited_until
            .get(&member.account_id)
            .copied()
            .map(Into::into);
        status_by_account
            .entry(member.account_id.as_str())
            .or_insert_with(|| resolve_account_status(&status, now).status);
        slots_by_account
            .entry(member.account_id.as_str())
            .or_insert(member.total_slots);
        accounts_by_group
            .entry(member.group_id.as_str())
            .or_default()
            .push(member.account_id.as_str());
    }
    for record in records {
        let account_ids = accounts_by_group
            .get(record.id.as_str())
            .map_or(&[][..], Vec::as_slice);
        let available_ids = account_ids
            .iter()
            .copied()
            .filter(|account_id| status_by_account.get(account_id) == Some(&AccountStatus::Normal))
            .collect::<Vec<_>>();
        let total = u64::try_from(account_ids.len()).unwrap_or(u64::MAX);
        let available = u64::try_from(available_ids.len()).unwrap_or(u64::MAX);
        record.account_summary = AccountGroupAccountSummary {
            available,
            limited: total.saturating_sub(available),
            total,
        };
        record.capacity = AccountGroupCapacity {
            used_slots: if available_ids.is_empty() {
                Some(0)
            } else {
                runtime.in_flight.as_ref().map(|in_flight| {
                    available_ids.iter().fold(0_u64, |sum, account_id| {
                        sum.saturating_add(in_flight.get(*account_id).copied().unwrap_or(0))
                    })
                })
            },
            total_slots: available_ids.iter().fold(0_u64, |sum, account_id| {
                sum.saturating_add(slots_by_account.get(*account_id).copied().unwrap_or(0))
            }),
        };
    }
}
