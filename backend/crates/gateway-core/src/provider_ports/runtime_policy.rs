//! Provider 运行策略端口及冻结、预热配置

use super::{ProviderRefreshPolicy, ProviderStoreError, ProviderStoreErrorKind};
use crate::{account::OpaqueProviderData, identity::ProviderKind, routing::ConfigRevision};
use futures::future::BoxFuture;

pub trait ProviderRuntimePolicyPort: Send + Sync {
    /// 原子推进预热执行游标，重启或时钟回拨后不重复领取已消费的时刻
    fn claim_warmup_slot<'a>(
        &'a self,
        _timezone: crate::time::DeploymentTimeZone,
        _slot: chrono::NaiveDateTime,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async {
            Err(ProviderStoreError::new(
                ProviderStoreErrorKind::Unavailable,
                "claim warmup slot",
            ))
        })
    }
    /// 仅首次启动写入该 Provider 的默认选择；已保存的管理配置始终优先
    fn initialize_request_profile<'a>(
        &'a self,
        _provider: &'a ProviderKind,
        initial: OpaqueProviderData,
    ) -> BoxFuture<'a, Result<OpaqueProviderData, ProviderStoreError>> {
        Box::pin(async move { Ok(initial) })
    }

    /// 读取候选配置版本实际引用的全局与 Client Key 画像配置
    ///
    /// 实现必须在同一数据库快照内核对 revision，且只返回画像投影，不能读取 Key
    /// 明文
    /// Provider 代次据此在发布前拒绝已失效的选择
    fn load_request_profile_configurations<'a>(
        &'a self,
        _revision: ConfigRevision,
        _provider: &'a ProviderKind,
    ) -> BoxFuture<'a, Result<Vec<OpaqueProviderData>, ProviderStoreError>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn load_refresh_policy(
        &self,
    ) -> BoxFuture<'_, Result<ProviderRefreshPolicy, ProviderStoreError>>;

    /// 读取账号模型预激活策略；默认关闭，只有实现运行时设置的存储需要覆盖
    fn load_warmup_policy(
        &self,
    ) -> BoxFuture<'_, Result<ProviderWarmupPolicy, ProviderStoreError>> {
        Box::pin(async move { Ok(ProviderWarmupPolicy::disabled()) })
    }
}

/// 校验每日预激活时间格式，如 "08:00" 或 "08:00,13:00"
#[must_use]
pub fn valid_warmup_schedule_time(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 255
        || value != value.trim()
        || value.chars().any(char::is_control)
    {
        return false;
    }
    for part in value.split(',') {
        let bytes = part.as_bytes();
        if bytes.len() != 5 || bytes[2] != b':' {
            return false;
        }
        let Ok(hour) = part[0..2].parse::<u32>() else {
            return false;
        };
        let Ok(minute) = part[3..5].parse::<u32>() else {
            return false;
        };
        if hour > 23 || minute > 59 {
            return false;
        }
    }
    true
}

/// 账号模型预激活（预热）策略；来源于 `runtime_settings`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderWarmupPolicy {
    enabled: bool,
    schedule_time: String,
    model: Option<String>,
}

impl ProviderWarmupPolicy {
    /// 保存设置与创建预热策略使用相同的时间表和模型约束
    pub fn validate_values(
        enabled: bool,
        schedule_time: &str,
        model: Option<&str>,
    ) -> Result<(), &'static str> {
        if !valid_warmup_schedule_time(schedule_time) {
            return Err("account_warmup_schedule_time");
        }
        if (enabled && model.is_none()) || !valid_optional_probe_model(model) {
            return Err("account_warmup_model");
        }
        Ok(())
    }

    pub fn try_new(
        enabled: bool,
        schedule_time: String,
        model: Option<String>,
    ) -> Result<Self, ProviderStoreError> {
        if Self::validate_values(enabled, &schedule_time, model.as_deref()).is_err() {
            return Err(ProviderStoreError::new(
                ProviderStoreErrorKind::InvalidData,
                "validate warmup policy",
            ));
        }
        Ok(Self {
            enabled,
            schedule_time,
            model,
        })
    }

    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            schedule_time: String::new(),
            model: None,
        }
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub fn schedule_time(&self) -> &str {
        &self.schedule_time
    }

    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// 解析每日时间列表，返回如 `vec![(8, 0)]`
    #[must_use]
    pub fn scheduled_times(&self) -> Vec<(u32, u32)> {
        self.schedule_time
            .split(',')
            .filter_map(|part| {
                let part = part.trim();
                let mut iter = part.split(':');
                let hour = iter.next()?.parse::<u32>().ok()?;
                let minute = iter.next()?.parse::<u32>().ok()?;
                Some((hour, minute))
            })
            .collect()
    }
}

fn valid_optional_probe_model(model: Option<&str>) -> bool {
    model.is_none_or(|model| {
        !model.is_empty()
            && model.len() <= 128
            && model == model.trim()
            && !model.bytes().any(|byte| byte.is_ascii_control())
    })
}
