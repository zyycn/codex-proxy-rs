//! 按 Key 持久化 USD 限额与已取得费用，不依赖尽力写入的请求观测

use std::time::SystemTime;

use futures::future::BoxFuture;

use crate::{error::GatewayError, metering::Decimal, policy::ClientApiKeyId};

use super::ModelRequestId;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientBudgetLimits {
    pub daily_usd: Decimal,
    pub weekly_usd: Decimal,
}

impl ClientBudgetLimits {
    #[must_use]
    pub fn is_limited(self) -> bool {
        self.daily_usd != Decimal::ZERO || self.weekly_usd != Decimal::ZERO
    }

    /// 准入与对外预算查询共用真实账本口径，展示字符串不参与访问判断
    #[must_use]
    pub fn access(self, daily_used: Decimal, weekly_used: Decimal) -> ClientBudgetAccess {
        ClientBudgetAccess {
            window_inactive: false,
            daily_limit_reached: self.daily_usd != Decimal::ZERO && daily_used >= self.daily_usd,
            weekly_limit_reached: self.weekly_usd != Decimal::ZERO
                && weekly_used >= self.weekly_usd,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientBudgetAccess {
    pub window_inactive: bool,
    pub daily_limit_reached: bool,
    pub weekly_limit_reached: bool,
}

impl ClientBudgetAccess {
    #[must_use]
    pub const fn allowed(self) -> bool {
        !self.window_inactive && !self.daily_limit_reached && !self.weekly_limit_reached
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientBudgetStatus {
    pub limits: ClientBudgetLimits,
    pub daily_used_usd: Decimal,
    pub weekly_used_usd: Decimal,
    pub daily_window: Option<ClientBudgetWindow>,
    pub weekly_window: Option<ClientBudgetWindow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientBudgetWindow {
    pub start: SystemTime,
    pub end: SystemTime,
    pub explicit: bool,
}

impl ClientBudgetWindow {
    /// 起点在释放控制后仍然有效，只有显式窗口在到期后停止准入
    #[must_use]
    pub fn inactive_at(self, now: SystemTime) -> bool {
        now < self.start || (self.explicit && now >= self.end)
    }

    #[must_use]
    pub fn visible_at(self, now: SystemTime) -> bool {
        self.explicit || (self.start < self.end && now < self.end)
    }
}

impl ClientBudgetStatus {
    #[must_use]
    pub fn access_at(&self, now: SystemTime) -> ClientBudgetAccess {
        ClientBudgetAccess {
            window_inactive: [self.daily_window, self.weekly_window]
                .into_iter()
                .flatten()
                .any(|w| w.inactive_at(now)),
            ..self
                .limits
                .access(self.daily_used_usd, self.weekly_used_usd)
        }
    }

    #[must_use]
    pub fn daily_resets_at(&self) -> Option<SystemTime> {
        self.daily_window.filter(|w| w.start < w.end).map(|w| w.end)
    }

    #[must_use]
    pub fn weekly_resets_at(&self) -> Option<SystemTime> {
        self.weekly_window
            .filter(|w| w.start < w.end)
            .map(|w| w.end)
    }
}

#[derive(Debug, Clone)]
pub struct ClientBudgetCharge {
    pub key_id: ClientApiKeyId,
    pub request_id: ModelRequestId,
    /// 已取得的 USD 费用，包含重试；缺少费用的尝试按零累计
    pub amount_usd: Decimal,
    pub completed_at: SystemTime,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("client budget store is unavailable")]
pub struct ClientBudgetError(#[source] pub Option<crate::error::ErrorSource>);

pub trait ClientBudgetPort: Send + Sync {
    /// 检查当前限额、已用金额与已接管的费用，不预占未来费用
    fn admit(&self, key_id: ClientApiKeyId) -> BoxFuture<'_, Result<(), GatewayError>>;

    /// 按网关请求 ID 幂等接管已取得费用；运行时入队后返回，由 Store 后台落库和重试
    fn settle(&self, charge: ClientBudgetCharge) -> BoxFuture<'_, Result<(), ClientBudgetError>>;
}
