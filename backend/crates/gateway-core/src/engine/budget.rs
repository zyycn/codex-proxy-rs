//! 按 Key 持久化 USD 限额与已取得费用，不依赖尽力写入的请求观测。

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
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientBudgetStatus {
    pub limits: ClientBudgetLimits,
    pub daily_used_usd: Decimal,
    pub weekly_used_usd: Decimal,
    pub daily_resets_at: Option<SystemTime>,
    pub weekly_resets_at: Option<SystemTime>,
}

#[derive(Debug, Clone)]
pub struct ClientBudgetCharge {
    pub key_id: ClientApiKeyId,
    pub request_id: ModelRequestId,
    /// 已取得的 USD 费用，包含重试；缺少费用的尝试按零累计。
    pub amount_usd: Decimal,
    pub completed_at: SystemTime,
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("client budget store is unavailable")]
pub struct ClientBudgetError;

pub trait ClientBudgetPort: Send + Sync {
    /// 原子检查当前限额与已用金额，不创建预扣费或待结算记录。
    fn admit(&self, key_id: ClientApiKeyId) -> BoxFuture<'_, Result<(), GatewayError>>;

    /// 按网关请求 ID 幂等累计已取得费用；写入失败由 Store 重试。
    fn settle(&self, charge: ClientBudgetCharge) -> BoxFuture<'_, Result<(), ClientBudgetError>>;
}
