use std::{
    collections::BTreeMap,
    str::FromStr as _,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use gateway_admin::{
    model::{
        MutationContext, PageSize, Revision,
        account_groups::{
            AccountGroupAccountSummary, AccountGroupCapacity, AccountGroupColor,
            AccountGroupListQuery, AccountGroupMemberFact, AccountGroupMutation, AccountGroupPage,
            AccountGroupRecord, AccountGroupUsage, DeleteAccountGroup, NewAccountGroup,
            SetAccountGroupEnabled, UpdateAccountGroup,
        },
        accounts::AccountRuntimeSnapshot,
        observability::DecimalAmount,
    },
    ports::store::{
        AccountGroupStore, AccountRuntimeStore, AdminStoreError, AdminStoreErrorKind,
        AdminStoreResult,
    },
};
use gateway_core::{
    account::{AccountStatusFacts, CredentialState, QuotaState},
    routing::AccountGroupId,
};

use super::AdminHarness;

const GROUP_ID: &str = "grp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[tokio::test]
async fn group_query_service_enriches_only_current_page_members_with_runtime_facts() {
    let groups = Arc::new(FakeGroupStore::default());
    let runtime = Arc::new(FakeRuntimeStore::default());
    let service = AdminHarness::new()
        .account_groups(groups.clone())
        .account_runtime(runtime.clone())
        .build()
        .await;

    let page = service
        .account_groups()
        .list(AccountGroupListQuery {
            page: 1,
            page_size: PageSize::new(20).expect("page size"),
            search: None,
            enabled: None,
        })
        .await
        .expect("list projected account groups");

    assert_eq!(
        groups
            .requested_groups
            .lock()
            .expect("requested groups")
            .as_slice(),
        [GROUP_ID]
    );
    assert_eq!(
        runtime
            .requested_accounts
            .lock()
            .expect("requested accounts")
            .as_slice(),
        ["acct_available", "acct_limited"]
    );
    let group = &page.items[0];
    assert_eq!(
        group.account_summary,
        AccountGroupAccountSummary {
            available: 1,
            limited: 1,
            total: 2,
        }
    );
    assert_eq!(
        group.capacity,
        AccountGroupCapacity {
            used_slots: Some(2),
            total_slots: Some(4),
        }
    );
}

#[derive(Default)]
struct FakeGroupStore {
    requested_groups: Mutex<Vec<String>>,
    members: Option<Vec<AccountGroupMemberFact>>,
}

#[tokio::test]
async fn group_capacity_only_becomes_unlimited_for_available_unlimited_members() {
    for (available_slots, unavailable_slots, expected) in
        [(None, Some(3), None), (Some(4), None, Some(4))]
    {
        let mut available = member("acct_available", 4);
        available.total_slots = available_slots;
        let mut unavailable = member("acct_limited", 3);
        unavailable.total_slots = unavailable_slots;
        let groups = Arc::new(FakeGroupStore {
            members: Some(vec![available, unavailable]),
            ..Default::default()
        });
        let service = AdminHarness::new()
            .account_groups(groups)
            .account_runtime(Arc::new(FakeRuntimeStore::default()))
            .build()
            .await;
        let page = service
            .account_groups()
            .list(AccountGroupListQuery {
                page: 1,
                page_size: PageSize::new(20).expect("page size"),
                search: None,
                enabled: None,
            })
            .await
            .expect("group capacity");
        assert_eq!(page.items[0].capacity.total_slots, expected);
        assert_eq!(page.items[0].capacity.used_slots, Some(2));
    }
}

#[async_trait]
impl AccountGroupStore for FakeGroupStore {
    async fn list_account_groups(
        &self,
        query: AccountGroupListQuery,
    ) -> AdminStoreResult<AccountGroupPage> {
        Ok(AccountGroupPage {
            config_revision: Revision::new(1).expect("revision"),
            items: vec![group_record()],
            total: 1,
            page: query.page,
            page_size: query.page_size.get(),
        })
    }

    async fn load_account_group_members(
        &self,
        group_ids: &[AccountGroupId],
    ) -> AdminStoreResult<Vec<AccountGroupMemberFact>> {
        *self.requested_groups.lock().expect("requested groups") = group_ids
            .iter()
            .map(|group_id| group_id.as_str().to_owned())
            .collect();
        Ok(self
            .members
            .clone()
            .unwrap_or_else(|| vec![member("acct_available", 4), member("acct_limited", 3)]))
    }

    async fn create_account_group(
        &self,
        _: NewAccountGroup,
        _: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation> {
        Err(unused())
    }

    async fn update_account_group(
        &self,
        _: UpdateAccountGroup,
        _: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation> {
        Err(unused())
    }

    async fn set_account_group_enabled(
        &self,
        _: SetAccountGroupEnabled,
        _: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation> {
        Err(unused())
    }

    async fn delete_account_group(
        &self,
        _: DeleteAccountGroup,
        _: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation> {
        Err(unused())
    }
}

#[derive(Default)]
struct FakeRuntimeStore {
    requested_accounts: Mutex<Vec<String>>,
}

#[async_trait]
impl AccountRuntimeStore for FakeRuntimeStore {
    async fn active_rate_limits(&self) -> AdminStoreResult<AccountRuntimeSnapshot> {
        Ok(AccountRuntimeSnapshot::default())
    }

    async fn account_runtime(
        &self,
        account_ids: &[String],
    ) -> AdminStoreResult<AccountRuntimeSnapshot> {
        *self.requested_accounts.lock().expect("requested accounts") = account_ids.to_vec();
        Ok(AccountRuntimeSnapshot {
            cooldown: BTreeMap::from([(
                "acct_limited".to_owned(),
                std::time::SystemTime::from(Utc::now() + Duration::minutes(5)).into(),
            )]),
            in_flight: Some(BTreeMap::from([("acct_available".to_owned(), 2)])),
        })
    }

    async fn active_freezes(
        &self,
    ) -> AdminStoreResult<BTreeMap<String, gateway_admin::model::accounts::AccountFreeze>> {
        Ok(BTreeMap::new())
    }

    async fn capacity_peaks(
        &self,
        _account_ids: &[String],
    ) -> AdminStoreResult<BTreeMap<String, u32>> {
        Ok(BTreeMap::new())
    }

    async fn finish_freeze(
        &self,
        _account_id: &str,
        _expected: &gateway_admin::model::accounts::AccountFreeze,
        _postpone_until: Option<DateTime<Utc>>,
    ) -> AdminStoreResult<bool> {
        Ok(false)
    }
}

fn group_record() -> AccountGroupRecord {
    let now = Utc::now();
    AccountGroupRecord {
        disable_fast: false,
        id: group_id(),
        name: "Primary".to_owned(),
        description: None,
        color: AccountGroupColor::parse("#2563EBFF").expect("color"),
        enabled: true,
        member_count: 2,
        provider_counts: BTreeMap::from([("openai".to_owned(), 2)]),
        client_key_count: 1,
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
            today_usd: DecimalAmount::from_str("1").expect("today usage"),
            retained_total_usd: DecimalAmount::from_str("3").expect("retained usage"),
        },
        created_at: now,
        updated_at: now,
    }
}

fn member(account_id: &str, total_slots: u64) -> AccountGroupMemberFact {
    AccountGroupMemberFact {
        group_id: group_id(),
        account_id: account_id.to_owned(),
        status: AccountStatusFacts {
            enabled: true,
            credential_state: CredentialState::Ready,
            access_token_expires_at: None,
            quota: QuotaState::default(),
            cooldown: None,
            last_error_reason: None,
            last_error_message: None,
        },
        total_slots: Some(total_slots),
    }
}

fn group_id() -> AccountGroupId {
    AccountGroupId::new(GROUP_ID).expect("group ID")
}

fn unused() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        "account group",
        "unused test operation",
    )
}
