//! 管理操作解析 Core 已发布的扩展集合；不持有独立的当前版本指针。

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock, Weak},
};

use gateway_core::{
    routing::{ProviderKind, RuntimeSnapshot},
    runtime::{RuntimeSnapshotHandle, extensions::ExtensionSetId},
};

use super::provider::{
    ProviderAdmin, ProviderAdminError, ProviderAdminErrorKind, ProviderAdminRegistry,
};

#[derive(Clone)]
pub struct ProviderAdminExtensionIndex {
    base: Arc<BTreeMap<ProviderKind, Arc<dyn ProviderAdmin>>>,
    sets: Arc<RwLock<BTreeMap<ExtensionSetId, Weak<ProviderAdminRegistry>>>>,
}

impl ProviderAdminExtensionIndex {
    #[must_use]
    pub fn new(base: ProviderAdminRegistry) -> Self {
        Self {
            base: base.providers,
            sets: Arc::default(),
        }
    }

    pub fn register(
        &self,
        id: ExtensionSetId,
        providers: impl IntoIterator<Item = Arc<dyn ProviderAdmin>>,
    ) -> Result<Arc<ProviderAdminRegistry>, ProviderAdminError> {
        let registry = Arc::new(ProviderAdminRegistry::new(
            self.base.values().cloned().chain(providers),
        )?);
        let mut sets = self
            .sets
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sets.retain(|_, registry| registry.strong_count() != 0);
        if sets.contains_key(&id) {
            return Err(ProviderAdminError::new(ProviderAdminErrorKind::Conflict));
        }
        sets.insert(id, Arc::downgrade(&registry));
        Ok(registry)
    }

    #[must_use]
    pub fn registry(&self, snapshots: RuntimeSnapshotHandle) -> ProviderAdminRegistry {
        ProviderAdminRegistry {
            providers: self.base.clone(),
            extensions: Some((self.clone(), snapshots)),
            snapshot: None,
        }
    }

    pub(super) fn resolve(
        &self,
        snapshot: Arc<RuntimeSnapshot>,
    ) -> Result<ProviderAdminRegistry, ProviderAdminError> {
        let reference = snapshot
            .extensions()
            .ok_or_else(|| ProviderAdminError::new(ProviderAdminErrorKind::Unavailable))?;
        if !reference.can_serve() {
            return Err(ProviderAdminError::new(ProviderAdminErrorKind::Unavailable));
        }
        let registry = self
            .sets
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(reference.id())
            .and_then(Weak::upgrade)
            .ok_or_else(|| ProviderAdminError::new(ProviderAdminErrorKind::Unavailable))?;
        Ok(ProviderAdminRegistry {
            providers: registry.providers.clone(),
            extensions: None,
            snapshot: Some(snapshot),
        })
    }
}

/// 管理操作及其后台观测保活同一集合，避免发布切换在 await 期间终止旧进程。
#[derive(Clone)]
pub struct ProviderAdminHandle {
    pub(super) provider: Arc<dyn ProviderAdmin>,
    pub(super) snapshot: Option<Arc<RuntimeSnapshot>>,
}

impl ProviderAdminHandle {
    /// 管理准备与 Core 探测共享完整快照，不能在两次 await 之间重新解析插件版本。
    #[must_use]
    pub fn snapshot(&self) -> Option<Arc<RuntimeSnapshot>> {
        self.snapshot.clone()
    }
}

impl std::ops::Deref for ProviderAdminHandle {
    type Target = dyn ProviderAdmin;
    fn deref(&self) -> &Self::Target {
        self.provider.as_ref()
    }
}

impl AsRef<dyn ProviderAdmin> for ProviderAdminHandle {
    fn as_ref(&self) -> &(dyn ProviderAdmin + 'static) {
        self.provider.as_ref()
    }
}
