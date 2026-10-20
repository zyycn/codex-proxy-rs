//! 当前路由快照的原子发布、冻结与健康投影

use crate::{
    health::{HealthProbe, HealthState},
    identity::ProviderKind,
    routing::{ConfigRevision, ProviderCatalogGeneration, RuntimeSnapshot},
};
use futures::future::BoxFuture;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex as SyncMutex, RwLock, Weak},
};

/// 请求级冻结失败；此状态必须 fail closed
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("runtime snapshot is unavailable")]
pub struct RuntimeSnapshotUnavailable;

/// RuntimeSnapshot 原子发布和请求级冻结句柄
#[derive(Clone, Default)]
pub struct RuntimeSnapshotHandle {
    current: Arc<RwLock<Option<Arc<RuntimeSnapshot>>>>,
    publications: Arc<SyncMutex<Vec<futures::channel::mpsc::Sender<()>>>>,
}

/// 不延长发布集合生命周期的只读句柄，供集合持有的扩展读取后续配置
#[derive(Clone, Default)]
pub struct WeakRuntimeSnapshotHandle {
    current: Weak<RwLock<Option<Arc<RuntimeSnapshot>>>>,
}

impl WeakRuntimeSnapshotHandle {
    pub fn acquire(&self) -> Result<Arc<RuntimeSnapshot>, RuntimeSnapshotUnavailable> {
        let current = self.current.upgrade().ok_or(RuntimeSnapshotUnavailable)?;
        acquire_current(&current)
    }
}

impl RuntimeSnapshotHandle {
    #[must_use]
    pub fn downgrade(&self) -> WeakRuntimeSnapshotHandle {
        WeakRuntimeSnapshotHandle {
            current: Arc::downgrade(&self.current),
        }
    }

    #[must_use]
    pub fn new(initial: RuntimeSnapshot) -> Self {
        Self {
            current: Arc::new(RwLock::new(Some(Arc::new(initial)))),
            publications: Default::default(),
        }
    }

    pub fn publish(&self, snapshot: RuntimeSnapshot) {
        *write_unpoisoned(&self.current) = Some(Arc::new(snapshot));
        self.notify_publication();
    }

    pub fn suspend(&self) {
        *write_unpoisoned(&self.current) = None;
        self.notify_publication();
    }

    /// 合并发布与暂停通知；订阅者收到后读取当前快照，不依赖逐条投递
    pub fn subscribe_publications(&self) -> futures::channel::mpsc::Receiver<()> {
        let (sender, receiver) = futures::channel::mpsc::channel(0);
        self.publications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(sender);
        receiver
    }

    fn notify_publication(&self) {
        self.publications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain_mut(|sender| match sender.try_send(()) {
                Ok(()) => true,
                Err(error) => error.is_full(),
            });
    }

    #[must_use]
    pub fn revision(&self) -> Option<ConfigRevision> {
        read_unpoisoned(&self.current)
            .as_ref()
            .map(|snapshot| snapshot.revision())
    }

    #[must_use]
    pub fn provider_catalog_generations(
        &self,
    ) -> Option<BTreeMap<ProviderKind, ProviderCatalogGeneration>> {
        read_unpoisoned(&self.current)
            .as_ref()
            .map(|snapshot| snapshot.provider_catalog_generations().clone())
    }

    /// 控制面只读诊断可观察尚未就绪的发布候选；数据面仍必须通过 [`Self::acquire`]
    #[must_use]
    pub fn snapshot_for_diagnostics(&self) -> Option<Arc<RuntimeSnapshot>> {
        read_unpoisoned(&self.current).clone()
    }

    /// 冻结当前 Arc；后续发布不改变已经开始的请求
    pub fn acquire(&self) -> Result<Arc<RuntimeSnapshot>, RuntimeSnapshotUnavailable> {
        acquire_current(&self.current)
    }
}

impl HealthProbe for RuntimeSnapshotHandle {
    fn name(&self) -> &'static str {
        "runtime_snapshot"
    }

    fn check(&self) -> BoxFuture<'_, HealthState> {
        Box::pin(async move {
            if self.acquire().is_ok() {
                HealthState::Healthy
            } else {
                HealthState::Unhealthy("Runtime snapshot is unavailable".to_owned())
            }
        })
    }
}

fn acquire_current(
    current: &RwLock<Option<Arc<RuntimeSnapshot>>>,
) -> Result<Arc<RuntimeSnapshot>, RuntimeSnapshotUnavailable> {
    read_unpoisoned(current)
        .clone()
        .filter(|snapshot| snapshot.extensions().is_none_or(|set| set.can_serve()))
        .ok_or(RuntimeSnapshotUnavailable)
}

fn read_unpoisoned<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write_unpoisoned<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
