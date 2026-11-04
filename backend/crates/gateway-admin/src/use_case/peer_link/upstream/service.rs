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
        UpstreamConnection, UpstreamInfoFailure, UpstreamInfoSource, UpstreamSchedulingSignal,
    },
    error::{StoreError, StoreErrorKind},
    routing::ProviderKind,
    task::WorkerCycleContext,
};
use gateway_protocol::peer::PeerLink;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex, RwLock},
    time::SystemTime,
};
use tokio::sync::Semaphore;

const REFRESH_CONCURRENCY: usize = 4;

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
            let accounts = match self.accounts.list_for_provider(provider).await {
                Ok(accounts) => accounts,
                Err(error) => {
                    failure.get_or_insert(error);
                    continue;
                }
            };
            let connections = accounts
                .iter()
                .filter(|account| self.supported(account))
                .map(UpstreamConnection::for_account)
                .collect::<Vec<_>>();
            active.extend(
                connections
                    .iter()
                    .map(|connection| connection.account_id.clone()),
            );
            let connections = {
                let mut cursors = self
                    .cursors
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let offset = cursors.entry(provider.clone()).or_default();
                if *offset >= connections.len() {
                    *offset = 0;
                }
                let batch = connections
                    .iter()
                    .skip(*offset)
                    .take(100)
                    .cloned()
                    .collect::<Vec<_>>();
                *offset += batch.len();
                batch
            };
            if let Err(error) = self.read_summaries(&connections).await {
                failure.get_or_insert(error);
                continue;
            }
            // 每轮有界并发，运行期到期记录在请求开始时预留，取消后可按普通重试恢复
            let pending = connections
                .into_iter()
                .filter(|connection| self.refresh_due(connection))
                .take(100)
                .collect::<Vec<_>>();
            stream::iter(pending).map(|connection| async move {
                if let Err(error) = self.refresh(&connection.account_id, false).await {
                    tracing::warn!(account_id = %connection.account_id, error = %error, "Upstream quota refresh failed");
                }
            }).buffer_unordered(REFRESH_CONCURRENCY).collect::<Vec<_>>().await;
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
}

impl UpstreamInfoSource for PeerInfoService {
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
