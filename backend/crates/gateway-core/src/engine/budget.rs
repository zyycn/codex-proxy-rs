//! Durable per-key USD budgets, independent of best-effort request observations.

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
    pub unresolved_requests: u64,
}

#[derive(Debug, Clone)]
pub struct ClientBudgetAdmission {
    pub key_id: ClientApiKeyId,
    pub request_id: ModelRequestId,
    pub deadline_at: SystemTime,
}

#[derive(Debug, Clone)]
pub struct ClientBudgetCharge {
    pub request_id: ModelRequestId,
    /// None means the request may have incurred an unknown charge, never zero.
    pub amount_usd: Option<Decimal>,
    pub completed_at: SystemTime,
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("client budget store is unavailable")]
pub struct ClientBudgetError;

pub trait ClientBudgetPort: Send + Sync {
    /// Persist a pending charge before any upstream work; read the current policy atomically.
    fn admit(&self, request: ClientBudgetAdmission) -> BoxFuture<'_, Result<(), GatewayError>>;

    /// Idempotent by gateway request ID. Failed writes leave the durable pending charge intact.
    fn settle(&self, charge: ClientBudgetCharge) -> BoxFuture<'_, Result<(), ClientBudgetError>>;
}
