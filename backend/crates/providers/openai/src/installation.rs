//! 按 Key 与账号解析持久 installation id，合并并发初始化并限制内存缓存容量

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gateway_core::account::ProviderAccountId;
use gateway_core::engine::AttemptContext;
use gateway_core::provider_ports::{
    ProviderClientInstallationKey, ProviderClientInstallationPort, ProviderStoreError,
    ProviderStoreErrorKind,
};
use tokio::sync::{Notify, OnceCell};
use uuid::Uuid;

use crate::transport::profile::identity::ResolvedRequestProfile;

const MAX_CACHED_INSTALLATIONS: usize = 4096;

pub(crate) struct CodexInstallationResolver {
    store: Arc<dyn ProviderClientInstallationPort>,
    entries: Mutex<HashMap<ProviderClientInstallationKey, Arc<OnceCell<Uuid>>>>,
    capacity_changed: Notify,
}

impl CodexInstallationResolver {
    pub(crate) fn new(store: Arc<dyn ProviderClientInstallationPort>) -> Self {
        Self {
            store,
            entries: Mutex::default(),
            capacity_changed: Notify::new(),
        }
    }

    pub(crate) async fn resolve(
        &self,
        attempt: &AttemptContext,
        account_id: &ProviderAccountId,
        account_installation_id: String,
    ) -> Result<String, ProviderStoreError> {
        let per_key = ResolvedRequestProfile::from_context(attempt)
            .map_err(|source| {
                ProviderStoreError::caused_by(
                    ProviderStoreErrorKind::InvalidData,
                    "openai_installation_profile",
                    source,
                )
            })?
            .is_some_and(|profile| {
                profile.independent_ua_configuration && profile.per_key_installation_id_enabled
            });
        if !per_key {
            return Ok(account_installation_id);
        }

        let key = ProviderClientInstallationKey::new(
            attempt.client_api_key_ref().clone(),
            account_id.clone(),
        );
        let entry = self.entry(key).await?;
        let installation_id = entry
            .value
            .get_or_try_init(|| async {
                if let Some(persisted) = self.store.read(&entry.release.key).await? {
                    return Ok::<Uuid, ProviderStoreError>(persisted);
                }
                self.store
                    .get_or_insert(&entry.release.key, Uuid::new_v4())
                    .await
            })
            .await?;
        Ok(installation_id.to_string())
    }

    async fn entry(
        &self,
        key: ProviderClientInstallationKey,
    ) -> Result<InstallationEntry<'_>, ProviderStoreError> {
        loop {
            let changed = self.capacity_changed.notified();
            tokio::pin!(changed);
            // 先登记通知，再检查容量，避免最后一个使用者在检查与等待之间退出
            changed.as_mut().enable();
            {
                let mut entries = self.entries.lock().map_err(|_| {
                    ProviderStoreError::new(
                        ProviderStoreErrorKind::Unavailable,
                        "openai_installation_cache",
                    )
                })?;
                if let Some(value) = entries.get(&key) {
                    return Ok(InstallationEntry {
                        value: Arc::clone(value),
                        release: InstallationCapacityRelease {
                            resolver: self,
                            key,
                        },
                    });
                }
                if entries.len() >= MAX_CACHED_INSTALLATIONS {
                    // 使用中的 cell 不能驱逐，否则同一 pair 会出现两次并发初始化
                    if let Some(idle) = entries
                        .iter()
                        .find(|(_, value)| Arc::strong_count(value) == 1)
                        .map(|(key, _)| key.clone())
                    {
                        entries.remove(&idle);
                    }
                }
                if entries.len() < MAX_CACHED_INSTALLATIONS {
                    let value = Arc::new(OnceCell::new());
                    entries.insert(key.clone(), Arc::clone(&value));
                    return Ok(InstallationEntry {
                        value,
                        release: InstallationCapacityRelease {
                            resolver: self,
                            key,
                        },
                    });
                }
            }
            changed.await;
        }
    }
}

struct InstallationEntry<'a> {
    value: Arc<OnceCell<Uuid>>,
    // 字段按声明顺序释放，通知必须晚于 cell 引用计数减少
    release: InstallationCapacityRelease<'a>,
}

struct InstallationCapacityRelease<'a> {
    resolver: &'a CodexInstallationResolver,
    key: ProviderClientInstallationKey,
}

impl Drop for InstallationCapacityRelease<'_> {
    fn drop(&mut self) {
        if let Ok(mut entries) = self.resolver.entries.lock()
            && entries
                .get(&self.key)
                .is_some_and(|value| value.get().is_none() && Arc::strong_count(value) == 1)
        {
            // 失败或取消不留下空结果，最后一个等待者退出后可重新读取持久映射
            entries.remove(&self.key);
        }
        self.resolver.capacity_changed.notify_waiters();
    }
}
