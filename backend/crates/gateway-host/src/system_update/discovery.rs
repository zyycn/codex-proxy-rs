//! 单个宿主实例共享发布索引和通道详情，版本探测只读内存

use gateway_admin::model::system::{
    SystemUpdateChannel as Channel, SystemUpdateCheck as Check, SystemUpdateDetail,
};
use semver::Version;
use std::{collections::VecDeque, sync::Arc, time::Duration};
use tokio::{
    sync::{Mutex, RwLock},
    time::{Instant, timeout},
};
use yaml_rust2::Yaml;

use super::diagnostics::Diagnostics;
use super::metadata::{Metadata, published_version};
use super::release::{
    GitHubRelease, base_update_detail, detail_from_release, update_target_allowed,
};
use super::{OperationError, SystemUpdateConfig, conflict, upstream};

const NORMAL: Duration = Duration::from_secs(1200);
const REFRESH: Duration = Duration::from_secs(60);
const BUDGET: Duration = Duration::from_secs(20);

struct Entry<T> {
    at: Instant,
    result: Result<T, OperationError>,
}

struct Slot<T> {
    value: RwLock<Option<Entry<T>>>,
    fetching: Mutex<()>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self {
            value: RwLock::new(None),
            fetching: Mutex::new(()),
        }
    }
}

impl<T: Clone> Slot<T> {
    async fn cached(&self, age: Duration) -> Option<Result<T, OperationError>> {
        let value = self.value.read().await;
        let entry = value.as_ref()?;
        let ttl = match &entry.result {
            Err(error) if error.retry_at().is_some_and(|at| at <= chrono::Utc::now()) => {
                return None;
            }
            Err(_) => REFRESH,
            Ok(_) => age,
        };
        (entry.at.elapsed() < ttl).then(|| entry.result.clone())
    }

    async fn get(
        &self,
        age: Duration,
        fetch: impl Future<Output = Result<T, OperationError>>,
        diagnostics: &Diagnostics,
    ) -> Result<(T, bool), OperationError> {
        if let Some(result) = self.cached(age).await {
            return result.map(|value| (value, true));
        }
        let guard = self.fetching.lock().await;
        if let Some(result) = self.cached(age).await {
            return result.map(|value| (value, true));
        }
        let result = fetch.await;
        *self.value.write().await = Some(Entry {
            at: Instant::now(),
            result: result.clone(),
        });
        drop(guard);
        if let Err(error) = &result {
            diagnostics.record("check_release", None, error).await;
        }
        result.map(|value| (value, false))
    }
}

type ReleaseSlot = Arc<Slot<Option<Arc<GitHubRelease>>>>;

#[derive(Default)]
pub(super) struct ReleaseCache {
    metadata: Metadata,
    index: Slot<Arc<Yaml>>,
    releases: Mutex<VecDeque<(Version, ReleaseSlot)>>,
}

impl ReleaseCache {
    pub(super) async fn local(
        &self,
        config: &SystemUpdateConfig,
        channel: Channel,
    ) -> SystemUpdateDetail {
        if let Some(reason) = config.update_support_error() {
            return base_update_detail(config, channel, Some(reason), None);
        }
        let Some(index) = self.index.cached(NORMAL).await else {
            return base_update_detail(config, channel, None, None);
        };
        let summary = index.and_then(|index| candidate(config, channel, &index));
        match summary {
            Ok((mut detail, _)) => {
                detail.cached = true;
                detail
            }
            Err(error) => base_update_detail(config, channel, None, Some(&error)),
        }
    }

    pub(super) async fn detail(
        &self,
        config: &SystemUpdateConfig,
        channel: Channel,
        check: Check,
        diagnostics: &Diagnostics,
    ) -> Result<SystemUpdateDetail, OperationError> {
        if let Some(reason) = config.update_support_error() {
            return Ok(base_update_detail(config, channel, Some(reason), None));
        }
        let age = if check == Check::Refresh {
            REFRESH
        } else {
            NORMAL
        };
        timeout(BUDGET, async {
            let (index, cached) = self
                .index
                .get(age, self.metadata.index(config), diagnostics)
                .await?;
            let (mut detail, target) = candidate(config, channel, &index)?;
            detail.cached = cached;
            if check != Check::Summary {
                let (release, reused) = self.exact(config, &target, age, diagnostics).await?;
                if let Some(release) = release {
                    detail = detail_from_release(config, &release, channel);
                }
                detail.cached = cached && reused;
            }
            Ok(detail)
        })
        .await
        .map_err(|_| upstream("检查更新超时"))?
    }

    async fn exact(
        &self,
        config: &SystemUpdateConfig,
        target: &Version,
        age: Duration,
        diagnostics: &Diagnostics,
    ) -> Result<(Option<Arc<GitHubRelease>>, bool), OperationError> {
        // 四个普通通道和当前版本最多需要五份详情，相同 tag 跨通道共享
        let slot = {
            let mut entries = self.releases.lock().await;
            let slot = entries
                .iter()
                .position(|(tag, _)| tag == target)
                .and_then(|i| entries.remove(i))
                .map(|(_, slot)| slot)
                .unwrap_or_default();
            if entries.len() == 5 {
                entries.pop_front();
            }
            entries.push_back((target.clone(), Arc::clone(&slot)));
            slot
        };
        slot.get(age, self.metadata.release(config, target), diagnostics)
            .await
    }

    pub(super) async fn install(
        &self,
        config: &SystemUpdateConfig,
        channel: Channel,
        confirmed: &str,
        diagnostics: &Diagnostics,
    ) -> Result<Arc<GitHubRelease>, OperationError> {
        // 执行时重新核对发布索引与精确 Release，不用浏览时的成功结果替代确认
        timeout(BUDGET, async {
            let (index, _) = self
                .index
                .get(Duration::ZERO, self.metadata.index(config), diagnostics)
                .await?;
            let (detail, target) = candidate(config, channel, &index)?;
            if !detail.has_update || detail.latest_version != confirmed {
                return Err(conflict("远端最新版本已变化，请重新检查并确认"));
            }
            self.exact(config, &target, Duration::ZERO, diagnostics)
                .await?
                .0
                .ok_or_else(|| upstream("发布索引指向的 Release 不存在"))
        })
        .await
        .map_err(|_| upstream("安装前版本复核超时"))?
    }
}

fn candidate(
    config: &SystemUpdateConfig,
    channel: Channel,
    index: &Yaml,
) -> Result<(SystemUpdateDetail, Version), OperationError> {
    let current = Version::parse(config.version.trim_start_matches('v'))
        .map_err(|_| conflict("当前版本号无效"))?;
    let target = published_version(index, &current, channel)?;
    let mut detail = base_update_detail(config, channel, None, None);
    if update_target_allowed(&current, &target, channel) {
        detail.latest_version = target.to_string();
        detail.has_update = true;
        Ok((detail, target))
    } else {
        Ok((detail, current))
    }
}
