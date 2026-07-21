//! 管理端账号连通性测试的真实执行链端口。

use futures::future::BoxFuture;

use crate::error::GatewayError;
use crate::routing::{ProviderInstanceId, UpstreamModelId};
use crate::{engine::credential::ProviderAccountId, operation::Operation};

#[derive(Debug, Clone, PartialEq)]
pub struct AccountProbeRequest {
    pub account_id: ProviderAccountId,
    pub provider_instance_id: ProviderInstanceId,
    pub upstream_model: UpstreamModelId,
    pub operation: Operation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountProbeResult {
    pub text: Vec<String>,
}

pub trait AccountProbe: Send + Sync {
    fn probe(
        &self,
        request: AccountProbeRequest,
    ) -> BoxFuture<'_, Result<AccountProbeResult, GatewayError>>;
}
