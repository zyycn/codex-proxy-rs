//! 后台计划协调与仅供 Smart 排序的内存观测

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{Duration, SystemTime},
};

use futures::{StreamExt as _, TryStreamExt as _, stream};
use gateway_core::{
    account::{
        LoadedCredential, ProviderAccount, ProviderAccountId, UpstreamConnection,
        UpstreamInfoClaim, UpstreamInfoCompletion, UpstreamInfoFailure, UpstreamInfoPlan,
        UpstreamInfoRecord, UpstreamInfoStore,
    },
    error::{StoreError, StoreErrorKind},
    routing::ProviderKind,
    task::WorkerCycleContext,
};

use gateway_admin::model::provider_credentials::ProviderQuota;
use gateway_protocol::peer::PeerLink;

use crate::credential::{CodexCredentialRepository, CodexRuntimeAuthentication};

use super::{
    client,
    scheduling::{PeerSchedulingSignal, SchedulingObservation},
    snapshot,
};

const BATCH_SIZE: u32 = 100;
const FETCH_BATCH: u32 = 20;
const SNAPSHOT_READ_BATCH: usize = 2;
const CONCURRENCY: usize = 4;
const MINIMUM_INTERVAL: Duration = Duration::from_secs(15);

type CachedSummary = (UpstreamInfoPlan, Option<Arc<PeerLink>>);

struct CachedObservation {
    connection: UpstreamConnection,
    completed_generation: u64,
    last_success_at: Option<SystemTime>,
    checked_at: SystemTime,
    scheduling: Option<Arc<SchedulingObservation>>,
    summary: Option<Arc<PeerLink>>,
}

impl CachedObservation {
    fn new(
        plan: &UpstreamInfoPlan,
        scheduling: Option<Arc<SchedulingObservation>>,
        summary: Option<Arc<PeerLink>>,
        now: SystemTime,
    ) -> Self {
        Self {
            connection: plan.connection.clone(),
            completed_generation: plan.completed_generation,
            last_success_at: plan.last_success_at,
            checked_at: now,
            scheduling,
            summary,
        }
    }

    fn matches(&self, plan: &UpstreamInfoPlan) -> bool {
        self.connection == plan.connection
            && self.completed_generation == plan.completed_generation
            && self.last_success_at == plan.last_success_at
            // 周期刷新不增加请求代次，撤销授权仍须立即丢弃此前的观测
            && (plan.last_error != Some(UpstreamInfoFailure::Unauthorized) || self.summary.is_none())
    }
}

pub(crate) struct PeerInfoService {
    repository: CodexCredentialRepository,
    store: Arc<dyn UpstreamInfoStore>,
    provider: ProviderKind,
    cursor: tokio::sync::Mutex<Option<ProviderAccountId>>,
    cache_refresh: tokio::sync::Mutex<()>,
    observations: RwLock<HashMap<ProviderAccountId, CachedObservation>>,
}

impl PeerInfoService {
    pub(crate) fn new(
        repository: CodexCredentialRepository,
        store: Arc<dyn UpstreamInfoStore>,
        provider: ProviderKind,
    ) -> Self {
        Self {
            repository,
            store,
            provider,
            cursor: tokio::sync::Mutex::new(None),
            cache_refresh: tokio::sync::Mutex::new(()),
            observations: RwLock::new(HashMap::new()),
        }
    }

    pub(crate) async fn account_changed(&self, id: &ProviderAccountId) -> Result<(), StoreError> {
        let loaded = self.repository.store().load_current_credential(id).await?;
        self.reconcile(&loaded).await
    }

    pub(crate) fn forget(&self, id: &ProviderAccountId) {
        let previous = self
            .observations
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
        drop(previous);
    }

    async fn reconcile(&self, loaded: &LoadedCredential) -> Result<(), StoreError> {
        let connection = UpstreamConnection::for_account(&loaded.account);
        let enabled = self
            .repository
            .decode_runtime_credential(loaded)
            .ok()
            .map(|credential| credential.authentication)
            .is_some_and(|credential| matches!(credential, CodexRuntimeAuthentication::ApiKey(_)));
        self.store
            .reconcile(
                &connection,
                enabled && loaded.account.enabled(),
                SystemTime::now(),
            )
            .await?;
        let _refresh = self.cache_refresh.lock().await;
        let mut observations = self
            .observations
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = if !enabled
            || !loaded.account.enabled()
            || observations
                .get(loaded.account.id())
                .is_some_and(|cached| cached.connection != connection)
        {
            observations.remove(loaded.account.id())
        } else {
            None
        };
        drop(observations);
        drop(previous);
        Ok(())
    }

    pub(crate) async fn request_refresh(
        &self,
        id: &ProviderAccountId,
    ) -> Result<UpstreamInfoPlan, StoreError> {
        let loaded = self.repository.store().load_current_credential(id).await?;
        if !matches!(
            self.repository
                .decode_runtime_credential(&loaded)
                .ok()
                .map(|credential| credential.authentication),
            Some(CodexRuntimeAuthentication::ApiKey(_))
        ) {
            return Err(StoreError::new(StoreErrorKind::InvalidData));
        }
        self.reconcile(&loaded).await?;
        self.store
            .request_refresh(
                &UpstreamConnection::for_account(&loaded.account),
                SystemTime::now(),
                MINIMUM_INTERVAL,
            )
            .await
    }

    pub(crate) async fn read(
        &self,
        ids: &[ProviderAccountId],
    ) -> Result<Vec<UpstreamInfoRecord>, StoreError> {
        self.store.read(ids).await
    }

    pub(super) async fn synchronize(&self, context: &WorkerCycleContext) -> Result<(), StoreError> {
        let now = SystemTime::now();
        // 过期观测保留轻量版本，避免每轮重读旧 JSON；长期未扫描到的账号释放缓存
        let expired = self
            .observations
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extract_if(|_, cached| {
                now.duration_since(cached.checked_at).unwrap_or_default() > snapshot::MAX_AGE
            })
            .collect::<Vec<_>>();
        drop(expired);
        let mut cursor = self.cursor.lock().await;
        let candidates = self
            .store
            .list_candidates(&self.provider, cursor.as_ref(), BATCH_SIZE)
            .await?;
        *cursor = if candidates.len() < BATCH_SIZE as usize {
            None
        } else {
            candidates.last().map(|loaded| loaded.account.id().clone())
        };
        drop(cursor);
        let mut ids = Vec::with_capacity(candidates.len());
        for loaded in candidates {
            if context.cancellation().is_cancelled() {
                return Ok(());
            }
            match self.reconcile(&loaded).await {
                Ok(()) => ids.push(loaded.account.id().clone()),
                Err(error)
                    if matches!(
                        error.kind(),
                        StoreErrorKind::Conflict | StoreErrorKind::InvalidData
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        self.refresh_cache(&ids).await?;
        let pending = self
            .store
            .claim_due(
                &self.provider,
                SystemTime::now(),
                FETCH_BATCH,
                Duration::from_secs(60),
            )
            .await?;
        stream::iter(pending)
            .map(|record| self.synchronize_one(record))
            .buffer_unordered(CONCURRENCY)
            .try_collect::<Vec<_>>()
            .await?;
        Ok(())
    }

    async fn synchronize_one(&self, claim: UpstreamInfoClaim) -> Result<(), StoreError> {
        let record = claim.plan;
        let loaded = match self
            .repository
            .store()
            .load_current_credential(&record.connection.account_id)
            .await
        {
            Ok(loaded) => loaded,
            Err(error)
                if matches!(
                    error.kind(),
                    StoreErrorKind::Conflict | StoreErrorKind::InvalidData
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if UpstreamConnection::for_account(&loaded.account) != record.connection
            || !loaded.account.enabled()
        {
            return self.reconcile(&loaded).await;
        }
        let Ok(CodexRuntimeAuthentication::ApiKey(authentication)) = self
            .repository
            .decode_runtime_credential(&loaded)
            .map(|credential| credential.authentication)
        else {
            return self.reconcile(&loaded).await;
        };
        let (result, retry_after) = match client::fetch(&loaded.account, &authentication).await {
            Ok(bytes) => (snapshot::parse(&bytes, SystemTime::now()), None),
            Err(failure) => (Err(failure.kind), failure.retry_after),
        };
        let now = SystemTime::now();
        let delay = match result {
            Ok(_) => Duration::from_secs(240 + stable_jitter(loaded.account.id().as_str())),
            Err(UpstreamInfoFailure::Unauthorized | UpstreamInfoFailure::Unsupported) => {
                Duration::from_secs(30 * 60)
            }
            Err(_) => Duration::from_secs(
                30_u64
                    .saturating_mul(1_u64 << record.consecutive_failures.min(6))
                    .min(30 * 60),
            ),
        }
        .max(retry_after.unwrap_or_default());
        let completion = UpstreamInfoCompletion {
            connection: record.connection.clone(),
            request_generation: record.request_generation,
            claim_token: claim.claim_token,
            completed_at: now,
            next_attempt_at: now + delay,
            result,
        };
        if self.store.complete(completion).await? {
            self.refresh_cache(std::slice::from_ref(loaded.account.id()))
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn read_summaries(
        &self,
        ids: &[ProviderAccountId],
    ) -> Result<Vec<(UpstreamConnection, ProviderQuota)>, StoreError> {
        let plans = self.store.read_plans(ids).await?;
        let cached = {
            let observations = self
                .observations
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            plans
                .into_iter()
                .map(|plan| {
                    let cached = observations.get(&plan.connection.account_id)?;
                    cached
                        .matches(&plan)
                        .then(|| (plan, cached.summary.clone()))
                })
                .collect::<Option<Vec<_>>>()
        };
        let summaries = match cached {
            Some(summaries) => summaries,
            None => self.refresh_cache(ids).await?,
        };
        let now = SystemTime::now();
        Ok(summaries
            .into_iter()
            .map(|(plan, summary)| {
                let quota = Self::project_peer(&plan, summary.as_deref(), now);
                (plan.connection, quota)
            })
            .collect())
    }

    async fn refresh_cache(
        &self,
        ids: &[ProviderAccountId],
    ) -> Result<Vec<CachedSummary>, StoreError> {
        let _refresh = self.cache_refresh.lock().await;
        let mut plans = self.store.read_plans(ids).await?;
        let now = SystemTime::now();
        let mut changed = Vec::new();
        let mut replaced = Vec::new();
        {
            let mut observations = self
                .observations
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for plan in &plans {
                if let Some(cached) = observations.get_mut(&plan.connection.account_id)
                    && cached.matches(plan)
                {
                    cached.checked_at = now;
                    continue;
                }
                if plan.last_success_at.is_none()
                    || plan.last_error == Some(UpstreamInfoFailure::Unauthorized)
                {
                    replaced.push(observations.insert(
                        plan.connection.account_id.clone(),
                        CachedObservation::new(plan, None, None, now),
                    ));
                } else {
                    changed.push(plan.connection.account_id.clone());
                }
            }
        }
        // 调度投影可能被其他请求持有，替换后的最后一次释放也留在锁外
        drop(replaced);
        // 完整观测可接近协议大小上限，投影后及时释放，避免同时物化整页 JSON
        for batch in changed.chunks(SNAPSHOT_READ_BATCH) {
            for record in self.store.read(batch).await? {
                if let Some(plan) = plans
                    .iter_mut()
                    .find(|plan| plan.connection.account_id == record.plan.connection.account_id)
                {
                    *plan = record.plan.clone();
                }
                self.cache(&record);
            }
        }
        let observations = self
            .observations
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(plans
            .into_iter()
            .map(|plan| {
                let summary = observations
                    .get(&plan.connection.account_id)
                    .filter(|cached| cached.matches(&plan))
                    .and_then(|cached| cached.summary.clone());
                (plan, summary)
            })
            .collect())
    }

    fn cache(&self, record: &UpstreamInfoRecord) {
        let now = SystemTime::now();
        let mut peer = record.snapshot.as_ref().and_then(snapshot::decode);
        let scheduling = peer
            .as_ref()
            .and_then(|peer| SchedulingObservation::new(peer, now))
            .filter(|observation| observation.fresh(now))
            .map(Arc::new);
        let summary = peer.take().map(|mut peer| {
            peer.quota = Default::default();
            peer.models = Vec::new();
            Arc::new(peer)
        });
        let cached = CachedObservation::new(&record.plan, scheduling, summary, now);
        let previous = self
            .observations
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(record.plan.connection.account_id.clone(), cached);
        drop(previous);
    }

    /// 数据面只取不可变投影；模型查询与有效期判断不持有共享锁
    pub(crate) fn signal(
        &self,
        account: &ProviderAccount,
        model: &str,
        now: SystemTime,
    ) -> Option<PeerSchedulingSignal> {
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

fn stable_jitter(id: &str) -> u64 {
    id.bytes().fold(0_u64, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(u64::from(byte))
    }) % 60
}
