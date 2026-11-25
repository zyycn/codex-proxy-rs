//! 发布包缓存的 CAS 与失败夹具，不依赖账号或认证资料

use futures::future::BoxFuture;
use gateway_core::{
    provider_ports::{
        ProviderArtifactProfile, ProviderArtifactProfileCachePort, ProviderStoreError,
        ProviderStoreErrorKind,
    },
    routing::ProviderKind,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Default)]
pub struct ArtifactCache {
    pub profiles: Mutex<BTreeMap<String, ProviderArtifactProfile>>,
    pub fail: AtomicBool,
    pub writes: AtomicUsize,
}
pub fn cache() -> Arc<ArtifactCache> {
    Arc::new(ArtifactCache::default())
}

impl ProviderArtifactProfileCachePort for ArtifactCache {
    fn replace_if_newer(
        &self,
        profile: ProviderArtifactProfile,
        _: Duration,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            if self.fail.load(Ordering::SeqCst) {
                return Err(ProviderStoreError::new(
                    ProviderStoreErrorKind::Unavailable,
                    "artifact cache",
                ));
            }
            let mut profiles = self.profiles.lock().unwrap();
            if let Some(old) = profiles.get(profile.artifact_key()) {
                if old.artifact_sequence() > profile.artifact_sequence() {
                    return Ok(false);
                }
                if old.artifact_sequence() == profile.artifact_sequence()
                    && old.profile() != profile.profile()
                {
                    return Err(ProviderStoreError::new(
                        ProviderStoreErrorKind::Conflict,
                        "artifact cache",
                    ));
                }
            }
            profiles.insert(profile.artifact_key().into(), profile);
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        })
    }
    fn read<'a>(
        &'a self,
        _: &'a ProviderKind,
        key: &'a str,
    ) -> BoxFuture<'a, Result<Option<ProviderArtifactProfile>, ProviderStoreError>> {
        Box::pin(async move {
            if self.fail.load(Ordering::SeqCst) {
                return Err(ProviderStoreError::new(
                    ProviderStoreErrorKind::Unavailable,
                    "artifact cache",
                ));
            }
            Ok(self.profiles.lock().unwrap().get(key).cloned())
        })
    }
}
