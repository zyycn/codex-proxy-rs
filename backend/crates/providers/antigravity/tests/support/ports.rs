//! 执行测试的最小内存端口，未使用的能力在被意外调用时失败

use super::{Lease, MemoryPorts};
use async_trait::async_trait;
use futures::future::BoxFuture;
use gateway_core::{
    account::*,
    error::{StoreError, StoreErrorKind},
    policy::ClientApiKeyId,
    provider_ports::*,
    routing::ProviderKind,
};
use std::{
    sync::atomic::Ordering,
    time::{Duration, SystemTime},
};

impl ProviderLeasePort for MemoryPorts {
    fn load_state<'a>(
        &'a self,
        _client_api_key_id: &'a ClientApiKeyId,
        _provider_kind: &'a ProviderKind,
        accounts: &'a [ProviderAccountId],
        _pool: ProviderConcurrencyPool,
    ) -> BoxFuture<'a, Result<ProviderSchedulingState, ProviderStoreError>> {
        Box::pin(async move {
            Ok(ProviderSchedulingState::new(
                accounts
                    .iter()
                    .map(|id| {
                        (
                            id.clone(),
                            AccountRuntimeSignals {
                                in_flight: self.accounts[id].active_leases.load(Ordering::SeqCst)
                                    as u32,
                                last_started_at: None,
                                quota_reset_at: None,
                                quota_reset_windows: None,
                                quota_remaining_rank: None,
                                cooldown: None,
                                failure_rate_basis_points: None,
                                first_output_latency_ms: None,
                            },
                        )
                    })
                    .collect(),
                self.scheduling_reads.fetch_add(1, Ordering::SeqCst) as u64,
            ))
        })
    }
    fn try_acquire(
        &self,
        request: ProviderLeaseRequest,
    ) -> BoxFuture<'_, Result<ProviderLeaseAcquisition, ProviderStoreError>> {
        Box::pin(async move {
            let (leases, limit) = match request {
                ProviderLeaseRequest::Scheduling(request) => (
                    &self.accounts[request.account_id()].active_leases,
                    request.max_concurrent().limit(),
                ),
                ProviderLeaseRequest::Refresh(_) | ProviderLeaseRequest::RefreshCapacity(_) => {
                    (&self.refresh_leases, None)
                }
            };
            if leases
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |active| {
                    limit
                        .is_none_or(|limit| active < limit.get() as usize)
                        .then_some(active + 1)
                })
                .is_err()
            {
                return Ok(ProviderLeaseAcquisition::Busy { retry_after: None });
            }
            Ok(ProviderLeaseAcquisition::Acquired(Box::new(Lease(
                leases.clone(),
            ))))
        })
    }
}

impl ProviderCatalogCachePort for MemoryPorts {
    fn replace<'a>(
        &'a self,
        _key: &'a ProviderCatalogCacheKey,
        _catalog: &'a OpaqueProviderData,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderCatalogCachePort::replace")
    }
    fn read<'a>(
        &'a self,
        _key: &'a ProviderCatalogCacheKey,
    ) -> BoxFuture<'a, Result<Option<OpaqueProviderData>, ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderCatalogCachePort::read")
    }
}

impl ProviderArtifactProfileCachePort for MemoryPorts {
    fn replace_if_newer(
        &self,
        _profile: ProviderArtifactProfile,
        _ttl: Duration,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderArtifactProfileCachePort::replace_if_newer")
    }
    fn read<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _artifact_key: &'a str,
    ) -> BoxFuture<'a, Result<Option<ProviderArtifactProfile>, ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderArtifactProfileCachePort::read")
    }
}

impl ProviderCredentialStatePort for MemoryPorts {
    fn replace(
        &self,
        _state: ProviderCredentialState,
    ) -> BoxFuture<'_, Result<(), ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderCredentialStatePort::replace")
    }
    fn read<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<ProviderCredentialState>, ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderCredentialStatePort::read")
    }
    fn clear<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderCredentialStatePort::clear")
    }
    fn record_refresh_backoff<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _window: Duration,
    ) -> BoxFuture<'a, Result<u32, ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderCredentialStatePort::record_refresh_backoff")
    }
    fn clear_refresh_backoff<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderCredentialStatePort::clear_refresh_backoff")
    }
}

impl ProviderRuntimePolicyPort for MemoryPorts {
    fn load_request_profile<'a>(
        &'a self,
        provider: &'a ProviderKind,
    ) -> BoxFuture<'a, Result<Option<OpaqueProviderData>, ProviderStoreError>> {
        Box::pin(async move {
            assert_eq!(provider.as_str(), "antigravity");
            Ok(self.request_profile.lock().unwrap().clone())
        })
    }

    fn load_refresh_policy(
        &self,
    ) -> BoxFuture<'_, Result<ProviderRefreshPolicy, ProviderStoreError>> {
        Box::pin(async {
            Ok(ProviderRefreshPolicy::try_new(
                Duration::from_secs(300),
                std::num::NonZeroU32::new(2).unwrap(),
            )
            .unwrap())
        })
    }
}

impl OAuthPendingFlowPort for MemoryPorts {
    fn put_if_absent(
        &self,
        _flow: NewOAuthPendingFlow,
    ) -> BoxFuture<'_, Result<OAuthPendingPutOutcome, ProviderStoreError>> {
        unreachable!("unused fixture port: OAuthPendingFlowPort::put_if_absent")
    }
    fn claim_if_owner<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _flow: &'a OAuthPendingBinding,
        _owner: &'a OAuthPendingBinding,
        _claim: &'a OAuthPendingBinding,
        _claim_ttl: Duration,
    ) -> BoxFuture<'a, Result<OAuthPendingClaimOutcome, ProviderStoreError>> {
        unreachable!("unused fixture port: OAuthPendingFlowPort::claim_if_owner")
    }
    fn release_claim<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _flow: &'a OAuthPendingBinding,
        _owner: &'a OAuthPendingBinding,
        _claim: &'a OAuthPendingBinding,
    ) -> BoxFuture<'a, Result<OAuthPendingReleaseOutcome, ProviderStoreError>> {
        unreachable!("unused fixture port: OAuthPendingFlowPort::release_claim")
    }
    fn consume_claim<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _flow: &'a OAuthPendingBinding,
        _owner: &'a OAuthPendingBinding,
        _claim: &'a OAuthPendingBinding,
    ) -> BoxFuture<'a, Result<OAuthPendingConsumeOutcome, ProviderStoreError>> {
        unreachable!("unused fixture port: OAuthPendingFlowPort::consume_claim")
    }
}

impl ProviderSessionAffinityPort for MemoryPorts {
    fn load<'a>(
        &'a self,
        provider: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
    ) -> BoxFuture<'a, Result<Option<ProviderSessionBinding>, ProviderStoreError>> {
        Box::pin(async move {
            assert_eq!(provider.as_str(), "antigravity");
            Ok(self
                .bindings
                .lock()
                .unwrap()
                .get(key)
                .map(|(binding, _)| binding.clone()))
        })
    }
    fn round_robin_cursor<'a>(
        &'a self,
        provider: &'a ProviderKind,
        client_key: &'a ClientApiKeyId,
    ) -> BoxFuture<'a, Result<u64, ProviderStoreError>> {
        Box::pin(async move {
            assert_eq!(provider.as_str(), "antigravity");
            Ok(self
                .assignment_cursors
                .lock()
                .unwrap()
                .get(client_key)
                .copied()
                .unwrap_or_default())
        })
    }
    fn admit_account<'a>(
        &'a self,
        provider: &'a ProviderKind,
        admission: ProviderAccountAdmission<'a>,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            assert_eq!(provider.as_str(), "antigravity");
            let count = self.claims.fetch_add(1, Ordering::SeqCst) + 1;
            if self.reject_next_claim.swap(false, Ordering::SeqCst) {
                return Ok(false);
            }
            let barrier = self.admission_barrier.lock().unwrap().clone();
            if let Some(barrier) = barrier {
                barrier.wait().await;
                self.admission_barrier.lock().unwrap().take();
            }
            let mut bindings = self.bindings.lock().unwrap();
            let mut cursors = self.assignment_cursors.lock().unwrap();
            let claim = admission.session();
            if claim.is_some_and(|claim| {
                bindings.get(claim.key).map(|(binding, _)| binding) != claim.expected
            }) {
                return Ok(false);
            }
            if let ProviderAccountAdmission::RoundRobin {
                client_key, cursor, ..
            } = admission
                && cursors.get(client_key).copied().unwrap_or_default() != cursor
            {
                return Ok(false);
            }
            if let Some(claim) = claim {
                let binding = match claim
                    .expected
                    .filter(|bound| bound.account_id() == claim.account)
                {
                    Some(bound) => bound.clone(),
                    None => {
                        ProviderSessionBinding::new(claim.account.clone(), format!("{count:032x}"))?
                    }
                };
                bindings.insert(claim.key.clone(), (binding, claim.ttl));
            }
            if let ProviderAccountAdmission::RoundRobin {
                client_key, cursor, ..
            } = admission
            {
                cursors.insert(client_key.clone(), cursor.wrapping_add(1));
            }
            Ok(true)
        })
    }
    fn load_alias<'a>(
        &'a self,
        _provider: &'a ProviderKind,
        _alias: &'a ProviderSessionAffinityKey,
    ) -> BoxFuture<'a, Result<Option<ProviderSessionAlias>, ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderSessionAffinityPort::load_alias")
    }
    fn bind_alias<'a>(
        &'a self,
        _provider: &'a ProviderKind,
        _alias: &'a ProviderSessionAffinityKey,
        _session: &'a ProviderSessionAlias,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderSessionAffinityPort::bind_alias")
    }
}

impl ProviderSessionExclusionPort for MemoryPorts {
    fn load<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _key: &'a ProviderSessionAffinityKey,
    ) -> BoxFuture<'a, Result<Option<ProviderSessionExclusions>, ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderSessionExclusionPort::load")
    }
    fn record_failure<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _key: &'a ProviderSessionAffinityKey,
        _account_id: &'a ProviderAccountId,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<ProviderSessionExclusions, ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderSessionExclusionPort::record_failure")
    }
    fn clear<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _key: &'a ProviderSessionAffinityKey,
        _expected_revision: &'a str,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderSessionExclusionPort::clear")
    }
}

impl ProviderCooldownPort for MemoryPorts {
    fn put_if_later(
        &self,
        _cooldown: ProviderCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderCooldownPort::put_if_later")
    }
    fn read<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<ProviderCooldown>, ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderCooldownPort::read")
    }
    fn read_many<'a>(
        &'a self,
        account_ids: &'a [ProviderAccountId],
    ) -> BoxFuture<'a, Vec<Result<Option<ProviderCooldown>, ProviderStoreError>>> {
        Box::pin(async move { account_ids.iter().map(|_| Ok(None)).collect() })
    }
    fn clear<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderCooldownPort::clear")
    }
    fn put_scoped_if_later(
        &self,
        cooldown: ProviderScopedCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            self.cooldowns.lock().unwrap().push(cooldown);
            Ok(true)
        })
    }
    fn read_scoped<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _scope: &'a ProviderCooldownScope,
    ) -> BoxFuture<'a, Result<Option<ProviderScopedCooldown>, ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderCooldownPort::read_scoped")
    }
    fn read_scoped_many<'a>(
        &'a self,
        account_ids: &'a [ProviderAccountId],
        scope: &'a ProviderCooldownScope,
    ) -> BoxFuture<'a, Vec<Result<Option<ProviderScopedCooldown>, ProviderStoreError>>> {
        Box::pin(async move {
            let values = self.cooldowns.lock().unwrap();
            account_ids
                .iter()
                .map(|id| {
                    Ok(values
                        .iter()
                        .find(|c| c.account_id() == id && c.scope() == scope)
                        .cloned())
                })
                .collect()
        })
    }
    fn clear_scoped<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _scope: &'a ProviderCooldownScope,
        _through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderCooldownPort::clear_scoped")
    }
    fn clear_all<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderCooldownPort::clear_all")
    }
    fn clear_after_success<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        unreachable!("unused fixture port: ProviderCooldownPort::clear_after_success")
    }
}

#[async_trait]
impl ProviderAccountStore for MemoryPorts {
    async fn expire_access_token(
        &self,
        account: &ProviderAccount,
        _: SystemTime,
    ) -> Result<bool, StoreError> {
        let matches = self.accounts.get(account.id()).is_some_and(|stored| {
            stored.loaded.lock().unwrap().account.revision() == account.revision()
        });
        if matches {
            self.expired.store(true, Ordering::SeqCst);
        }
        Ok(matches)
    }

    async fn load_account_catalogs(
        &self,
        _provider: &ProviderKind,
    ) -> Result<Vec<StoredAccountCatalog>, StoreError> {
        Ok(self
            .accounts
            .values()
            .filter_map(|account| {
                let loaded = account.loaded.lock().unwrap();
                let catalog = account.catalog.lock().unwrap();
                let (observation, document) = catalog.as_ref()?;
                (loaded.account.enabled()
                    && observation.revision == loaded.account.revision()
                    && observation.plan.as_deref() == loaded.account.plan_type())
                .then(|| StoredAccountCatalog {
                    accounts: vec![observation.clone()],
                    document: document.clone(),
                })
            })
            .collect())
    }
    async fn save_account_catalog(
        &self,
        observation: &AccountCatalogObservation,
        document: &[u8],
    ) -> Result<bool, StoreError> {
        let Some(account) = self.accounts.get(&observation.account_id) else {
            return Ok(false);
        };
        let loaded = account.loaded.lock().unwrap();
        if observation.revision != loaded.account.revision()
            || observation.plan.as_deref() != loaded.account.plan_type()
        {
            return Ok(false);
        }
        *account.catalog.lock().unwrap() =
            Some((observation.clone(), bytes::Bytes::copy_from_slice(document)));
        Ok(true)
    }
    async fn create_account(&self, _account: NewProviderAccount) -> Result<(), StoreError> {
        unreachable!("unused fixture port: ProviderAccountStore::create_account")
    }
    async fn get_account(
        &self,
        account: &ProviderAccountId,
    ) -> Result<Option<ProviderAccount>, StoreError> {
        Ok(self
            .accounts
            .get(account)
            .map(|account| account.loaded.lock().unwrap().account.clone()))
    }
    async fn list_accounts(&self) -> Result<Vec<ProviderAccount>, StoreError> {
        Ok(self
            .accounts
            .values()
            .map(|account| account.loaded.lock().unwrap().account.clone())
            .collect())
    }
    async fn list_for_provider(
        &self,
        provider: &ProviderKind,
        include_disabled: bool,
    ) -> Result<Vec<ProviderAccount>, StoreError> {
        Ok(self
            .list_accounts()
            .await?
            .into_iter()
            .filter(|account| {
                account.provider() == provider && (include_disabled || account.enabled())
            })
            .collect())
    }
    async fn list_for_provider_accounts(
        &self,
        _provider: &ProviderKind,
        account_ids: &[ProviderAccountId],
    ) -> Result<Vec<ProviderAccount>, StoreError> {
        Ok(self
            .list_accounts()
            .await?
            .into_iter()
            .filter(|account| account.enabled() && account_ids.contains(account.id()))
            .collect())
    }
    async fn list_refresh_candidates(
        &self,
        _query: ProviderRefreshQuery,
    ) -> Result<Vec<LoadedCredential>, StoreError> {
        unreachable!("unused fixture port: ProviderAccountStore::list_refresh_candidates")
    }
    async fn load_credential(
        &self,
        account: &ProviderAccountId,
        expected_revision: CredentialRevision,
    ) -> Result<LoadedCredential, StoreError> {
        let loaded = self.load_current_credential(account).await?;
        assert_eq!(loaded.account.revision(), expected_revision);
        Ok(loaded)
    }
    async fn load_current_credential(
        &self,
        account: &ProviderAccountId,
    ) -> Result<LoadedCredential, StoreError> {
        let account = self
            .accounts
            .get(account)
            .ok_or_else(|| StoreError::new(StoreErrorKind::InvalidState))?;
        Ok(account.loaded.lock().unwrap().clone())
    }
    async fn compare_and_swap_credential(
        &self,
        _update: CredentialCasUpdate,
    ) -> Result<CredentialCasOutcome, StoreError> {
        unreachable!("unused fixture port: ProviderAccountStore::compare_and_swap_credential")
    }
    async fn get_quotas(
        &self,
        accounts: &[ProviderAccountId],
    ) -> Result<Vec<QuotaObservation>, StoreError> {
        Ok(accounts
            .iter()
            .filter_map(|id| self.accounts.get(id)?.quota.lock().unwrap().clone())
            .collect())
    }
    async fn compare_and_swap_quota(
        &self,
        observation: QuotaObservation,
    ) -> Result<QuotaWriteOutcome, StoreError> {
        let Some(account) = self.accounts.get(&observation.account_id) else {
            return Ok(QuotaWriteOutcome::Conflict);
        };
        let mut current = account.loaded.lock().unwrap();
        let mut quota = account.quota.lock().unwrap();
        if current.account.revision() != observation.expected_revision
            || quota
                .as_ref()
                .is_some_and(|q| q.observed_at > observation.observed_at)
        {
            return Ok(QuotaWriteOutcome::Conflict);
        }
        current.account = current
            .account
            .clone()
            .with_profile(
                current.account.email().map(str::to_owned),
                current.account.upstream_account_id().map(str::to_owned),
                observation.plan_type.clone(),
            )
            .with_account_facts(
                current.account.enabled(),
                current.account.credential_state(),
                observation.state,
                None,
                None,
            );
        *quota = Some(observation);
        Ok(QuotaWriteOutcome::Updated)
    }
    async fn touch_quota_observation(
        &self,
        _touch: QuotaObservationTouch,
    ) -> Result<QuotaWriteOutcome, StoreError> {
        unreachable!("unused fixture port: ProviderAccountStore::touch_quota_observation")
    }
    async fn apply_quota_access(
        &self,
        _change: QuotaAccessChange,
    ) -> Result<QuotaWriteOutcome, StoreError> {
        unreachable!("unused fixture port: ProviderAccountStore::apply_quota_access")
    }
    async fn apply_state_change(&self, _change: AccountStateChange) -> Result<(), StoreError> {
        unreachable!("unused fixture port: ProviderAccountStore::apply_state_change")
    }
    async fn update_account(&self, _update: ProviderAccountUpdate) -> Result<(), StoreError> {
        unreachable!("unused fixture port: ProviderAccountStore::update_account")
    }
    async fn set_enabled(
        &self,
        account: &ProviderAccountId,
        enabled: bool,
    ) -> Result<(), StoreError> {
        let entry = self
            .accounts
            .get(account)
            .ok_or_else(|| StoreError::new(StoreErrorKind::InvalidState))?;
        let mut loaded = entry.loaded.lock().unwrap();
        let account = &loaded.account;
        loaded.account = account.clone().with_account_facts(
            enabled,
            account.credential_state(),
            account.quota(),
            None,
            None,
        );
        Ok(())
    }
    async fn delete_account(&self, _account: &ProviderAccountId) -> Result<(), StoreError> {
        unreachable!("unused fixture port: ProviderAccountStore::delete_account")
    }
}

#[async_trait]
impl gateway_core::diagnostics::OperationalDiagnostics for MemoryPorts {
    async fn record_failure(
        &self,
        _: gateway_core::diagnostics::OperationalFailure,
    ) -> Result<(), StoreError> {
        Ok(())
    }
}
