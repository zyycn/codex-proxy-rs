//! 管理员按额度窗口设置的使用上限与到期预热策略

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(try_from = "BTreeMap<String, QuotaBucketSettings>")]
pub struct AccountQuotaSettings(BTreeMap<String, QuotaBucketSettings>);

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuotaBucketSettings {
    pub limit_percent: Option<u8>,
    pub warmup: Option<QuotaBucketWarmup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaBucketWarmup {
    pub model: String,
}

impl AccountQuotaSettings {
    #[must_use]
    pub const fn disabled() -> Self {
        Self(BTreeMap::new())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &QuotaBucketSettings)> {
        self.0
            .iter()
            .map(|(key, settings)| (key.as_str(), settings))
    }

    #[must_use]
    pub fn has_warmup(&self) -> bool {
        self.0.values().any(|settings| settings.warmup.is_some())
    }

    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    #[must_use]
    pub fn reached(&self, key: &str, used: Option<f64>) -> bool {
        self.0
            .get(key)
            .and_then(|settings| settings.limit_percent)
            .zip(used)
            .is_some_and(|(limit, used)| used >= f64::from(limit))
    }
}

impl TryFrom<BTreeMap<String, QuotaBucketSettings>> for AccountQuotaSettings {
    type Error = &'static str;

    fn try_from(limits: BTreeMap<String, QuotaBucketSettings>) -> Result<Self, Self::Error> {
        if limits.iter().any(|(key, settings)| {
            key.is_empty()
                || key.len() > 256
                || key.chars().any(char::is_control)
                || settings
                    .limit_percent
                    .is_some_and(|limit| !(1..=100).contains(&limit))
                || settings.warmup.as_ref().is_some_and(|warmup| {
                    warmup.model.is_empty()
                        || warmup.model.len() > 128
                        || warmup.model.trim() != warmup.model
                        || warmup.model.chars().any(char::is_control)
                })
        }) {
            return Err("额度桶设置无效");
        }
        Ok(Self(limits))
    }
}

/// 合并时读到的快照版本，时间为空表示当时尚无快照
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaMergeBase {
    pub observed_at: Option<std::time::SystemTime>,
}

/// 此次判断依据的策略，以及是否有通用窗口触发暂停
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaPauseDecision {
    /// 独立观测无需合并，依赖旧快照的判断必须随写入校验版本
    pub merge_base: Option<QuotaMergeBase>,
    pub limits: AccountQuotaSettings,
    pub paused: bool,
}
