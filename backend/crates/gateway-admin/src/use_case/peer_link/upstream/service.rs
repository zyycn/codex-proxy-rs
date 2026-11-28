//! 互联额度读取与后台刷新，复用账号额度存储和 Host Worker 生命周期

use super::{refresh::RefreshState, scheduling::SchedulingObservation, snapshot};
use crate::{
    model::provider_credentials::ProviderQuota,
    ports::{
        peer::{PeerConnection, PeerTransport},
        provider::{ProviderAdmin, ProviderAdminRegistry},
    },
};
use futures::{StreamExt as _, stream};
use gateway_core::{
    account::{
        LoadedCredential, ProviderAccount, ProviderAccountId, ProviderAccountStore,
        QuotaObservationVersion, UpstreamConnection, UpstreamInfoFailure, UpstreamInfoSource,
        UpstreamSchedulingSignal,
    },
    error::{StoreError, StoreErrorKind},
    routing::ProviderKind,
    task::WorkerCycleContext,
};
use gateway_protocol::peer::PeerLink;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, SystemTime},
};
use tokio::sync::Semaphore;

const REFRESH_CONCURRENCY: usize = 4;
const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

pub(super) struct CachedObservation {
    pub connection: UpstreamConnection,
    pub observed_at: Option<SystemTime>,
    pub checked_at: SystemTime,
    pub scheduling: Option<Arc<SchedulingObservation>>,
    pub summary: Option<Arc<PeerLink>>,
    pub stored_failure: Option<UpstreamInfoFailure>,
}

pub(crate) struct PeerInfoService {
    pub(super) accounts: Arc<dyn ProviderAccountStore>,
    providers: BTreeMap<ProviderKind, Arc<dyn ProviderAdmin>>,
    pub(super) transport: Arc<dyn PeerTransport>,
    pub(super) attempts: Mutex<HashMap<ProviderAccountId, Arc<tokio::sync::Mutex<RefreshState>>>>,
    cursors: Mutex<BTreeMap<ProviderKind, usize>>,
    pub(super) cache_refresh: tokio::sync::Mutex<()>,
    pub(super) refresh_slots: Semaphore,
    pub(super) observations: RwLock<HashMap<ProviderAccountId, CachedObservation>>,
}

impl PeerInfoService {
    pub(crate) fn new(
        accounts: Arc<dyn ProviderAccountStore>,
        providers: &ProviderAdminRegistry,
        transport: Arc<dyn PeerTransport>,
    ) -> Self {
        Self {
            accounts,
            providers: providers.registered(),
            transport,
            attempts: Mutex::default(),
            cursors: Mutex::default(),
            cache_refresh: tokio::sync::Mutex::new(()),
            refresh_slots: Semaphore::new(REFRESH_CONCURRENCY),
            observations: RwLock::default(),
        }
    }

    pub(crate) async fn account_changed(&self, id: &ProviderAccountId) -> Result<(), StoreError> {
        let has_observation = self
            .observations
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(id);
        let has_attempt = self
            .attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(id);
        if !has_observation && !has_attempt {
            return Ok(());
        }
        let Some(account) = self
            .accounts
            .get_account(id)
            .await?
            .filter(|account| self.supported(account))
        else {
            self.forget(id);
            return Ok(());
        };
        let connection = UpstreamConnection::for_account(&account);
        // 普通信息编辑保留观测与退避，连接变化由刷新状态中的身份比较处理
        let mut observations = self
            .observations
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if observations
            .get(id)
            .is_some_and(|cached| cached.connection != connection)
        {
            observations.remove(id);
        }
        Ok(())
    }

    pub(crate) fn forget(&self, id: &ProviderAccountId) {
        let previous = self
            .observations
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
        drop(previous);
        // 正在执行的请求继续持有同一把锁，结果仍由数据库版本校验决定是否接纳
        let mut attempts = self
            .attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if attempts
            .get(id)
            .is_some_and(|state| Arc::strong_count(state) == 1)
        {
            attempts.remove(id);
        }
    }

    pub(super) fn connection(&self, loaded: &LoadedCredential) -> Option<PeerConnection> {
        self.providers
            .get(loaded.account.provider())?
            .peer_connection(loaded)
            .ok()
            .flatten()
    }

    fn supported(&self, account: &ProviderAccount) -> bool {
        account.enabled()
            && self
                .providers
                .get(account.provider())
                .is_some_and(|provider| {
                    provider.supports_peer_connection(account.authentication_kind())
                })
    }

    async fn current_connection(
        &self,
        id: &ProviderAccountId,
    ) -> Result<Option<UpstreamConnection>, StoreError> {
        let account = self
            .accounts
            .get_account(id)
            .await?
            .ok_or_else(|| StoreError::new(StoreErrorKind::InvalidData))?;
        Ok(self
            .supported(&account)
            .then(|| UpstreamConnection::for_account(&account)))
    }

    pub(crate) async fn read_quota(
        &self,
        id: &ProviderAccountId,
    ) -> Result<Option<ProviderQuota>, StoreError> {
        let Some(connection) = self.current_connection(id).await? else {
            return Ok(None);
        };
        let observation = self
            .accounts
            .get_quotas(std::slice::from_ref(id))
            .await?
            .into_iter()
            .find(|value| value.expected_revision == connection.credential_revision);
        let peer = observation
            .as_ref()
            .and_then(|value| snapshot::decode(&value.quota));
        let stored_failure = observation
            .as_ref()
            .and_then(|value| snapshot::stored_failure(&value.quota, &connection));
        if self.current_connection(id).await?.as_ref() != Some(&connection) {
            return Ok(None);
        }
        Ok(Some(Self::project_peer(
            &connection,
            peer.as_ref(),
            stored_failure,
            self.last_error(&connection),
            SystemTime::now(),
        )))
    }

    pub(crate) async fn read_summary(
        &self,
        id: &ProviderAccountId,
    ) -> Result<Option<ProviderQuota>, StoreError> {
        let Some(connection) = self.current_connection(id).await? else {
            return Ok(None);
        };
        Ok(self
            .read_summaries(&[connection])
            .await?
            .into_iter()
            .next()
            .map(|(_, quota)| quota))
    }

    pub(super) async fn synchronize(&self, context: &WorkerCycleContext) -> Result<(), StoreError> {
        let mut failure = None;
        let mut active = std::collections::HashSet::new();
        for provider in self.providers.keys() {
            if context.cancellation().is_cancelled() {
                break;
            }
            let mut accounts = match self.accounts.list_for_provider(provider, false).await {
                Ok(accounts) => accounts,
                Err(error) => {
                    failure.get_or_insert(error);
                    continue;
                }
            };
            accounts.retain(|account| self.supported(account));
            active.extend(accounts.iter().map(|account| account.id().clone()));
            if accounts.is_empty() {
                continue;
            }
            let batch = {
                let mut cursors = self
                    .cursors
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let offset = cursors.entry(provider.clone()).or_default();
                if *offset >= accounts.len() {
                    *offset = 0;
                }
                let end = (*offset + 100).min(accounts.len());
                let batch = &accounts[*offset..end];
                *offset = end;
                batch
            };
            if let Err(error) = self.refresh_batch(batch).await {
                failure.get_or_insert(error);
            }
        }
        // 查询失败时保留该轮无法核对的账号，避免错误风暴清空所有节流记录
        if failure.is_none() && !context.cancellation().is_cancelled() {
            self.attempts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|id, state| active.contains(id) || Arc::strong_count(state) > 1);
        }
        let now = SystemTime::now();
        let removed = self
            .observations
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extract_if(|_, cached| {
                now.duration_since(cached.checked_at).unwrap_or_default() > snapshot::MAX_AGE
            })
            .collect::<Vec<_>>();
        drop(removed);
        failure.map_or(Ok(()), Err)
    }

    async fn refresh_batch(&self, accounts: &[ProviderAccount]) -> Result<(), StoreError> {
        let connections: Vec<_> = accounts
            .iter()
            .map(UpstreamConnection::for_account)
            .collect();
        self.hydrate(&connections).await?;
        let now = SystemTime::now();
        let mut due = Vec::new();
        let mut stale = Vec::new();
        {
            let observations = self
                .observations
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (account, connection) in accounts.iter().zip(connections) {
                let cached = observations
                    .get(account.id())
                    .filter(|c| c.connection == connection);
                if cached
                    .is_some_and(|c| c.stored_failure == Some(UpstreamInfoFailure::Unsupported))
                    || !self.retry_ready(&connection)
                {
                    continue;
                }
                // 只有成功识别的快照参与惰性刷新，授权失败等识别记录仍按退避复查
                let Some(observed_at) = cached
                    .filter(|c| c.summary.is_some())
                    .and_then(|c| c.observed_at)
                else {
                    due.push(connection.account_id);
                    continue;
                };
                let aged = now.duration_since(observed_at).unwrap_or_default() >= REFRESH_INTERVAL;
                let quota = account.quota();
                if quota.is_exhausted() {
                    // 已在重置后查过但未获恢复证据时，等下一个复查周期
                    if quota.exhaustion_refresh_due(now, REFRESH_INTERVAL)
                        && (aged || quota.reset_at().is_some_and(|reset| observed_at < reset))
                    {
                        due.push(connection.account_id);
                    }
                } else if aged {
                    stale.push(QuotaObservationVersion {
                        account_id: connection.account_id,
                        credential_revision: connection.credential_revision,
                        observed_at,
                    });
                }
            }
        }
        // 用量查询失败只影响正常账号，首查与耗尽复查仍可执行
        let mut outcome = Ok(());
        if !stale.is_empty() {
            match self.accounts.accounts_used_since_quota(&stale).await {
                Ok(active) => due.extend(active),
                Err(error) => outcome = Err(error),
            }
        }
        stream::iter(due)
            .for_each_concurrent(REFRESH_CONCURRENCY, |id| async move {
                if let Err(error) = self.refresh(&id, false).await {
                    tracing::warn!(account_id = %id, error = %error, "Upstream quota refresh failed");
                }
            })
            .await;
        outcome
    }
}

impl UpstreamInfoSource for PeerInfoService {
    fn is_peer(&self, account: &ProviderAccount) -> bool {
        if !self.supported(account) {
            return false;
        }
        let connection = UpstreamConnection::for_account(account);
        self.observations
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(account.id())
            .is_some_and(|cached| {
                cached.connection == connection
                    && snapshot::is_discovered(cached.summary.as_deref(), cached.stored_failure)
            })
    }

    fn signal(
        &self,
        account: &ProviderAccount,
        model: Option<&str>,
        now: SystemTime,
    ) -> Option<UpstreamSchedulingSignal> {
        if !account.enabled() {
            return None;
        }
        let connection = UpstreamConnection::for_account(account);
        let scheduling = {
            let observations = self
                .observations
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let cached = observations.get(account.id())?;
            if cached.connection != connection {
                return None;
            }
            Arc::clone(cached.scheduling.as_ref()?)
        };
        scheduling.signal(model, now)
    }
}
