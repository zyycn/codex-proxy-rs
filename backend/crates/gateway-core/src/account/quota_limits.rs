//! 管理员按账号通用窗口设置的已用比例上限

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(try_from = "BTreeMap<String, u8>")]
pub struct AccountQuotaLimits(BTreeMap<String, u8>);

impl AccountQuotaLimits {
    #[must_use]
    pub const fn disabled() -> Self {
        Self(BTreeMap::new())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
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
            .zip(used)
            .is_some_and(|(&limit, used)| used >= f64::from(limit))
    }
}

impl TryFrom<BTreeMap<String, u8>> for AccountQuotaLimits {
    type Error = &'static str;

    fn try_from(limits: BTreeMap<String, u8>) -> Result<Self, Self::Error> {
        if limits.iter().any(|(key, limit)| {
            key.is_empty()
                || key.len() > 256
                || key.chars().any(char::is_control)
                || !(1..=100).contains(limit)
        }) {
            return Err("额度窗口标识无效或上限不在 1～100 范围内");
        }
        Ok(Self(limits))
    }
}

/// 此次判断依据的策略，以及是否有通用窗口触发暂停
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaPauseDecision {
    pub limits: AccountQuotaLimits,
    pub paused: bool,
}
