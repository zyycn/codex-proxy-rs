//! 账号管理与配额刷新用例拥有的 OpenAI IO 端口。

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use secrecy::SecretString;
use serde_json::Value;

use crate::fleet::quota::QuotaSnapshot;

#[derive(Debug, Clone)]
pub struct AccountUpstreamContext {
    pub access_token: SecretString,
    pub account_id: Option<String>,
    pub request_id: String,
    pub cookie_header: Option<String>,
    pub installation_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountFailureObservation {
    pub status_code: Option<u16>,
    pub code: Option<String>,
    pub error_type: Option<String>,
    pub identity_authorization_error: Option<String>,
    pub identity_error_code: Option<String>,
    pub message: String,
    pub body: String,
    pub retry_after_seconds: Option<u64>,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct AccountGatewayError {
    message: String,
    failure: Option<AccountFailureObservation>,
}

impl AccountGatewayError {
    pub fn new(message: impl Into<String>, failure: Option<AccountFailureObservation>) -> Self {
        Self {
            message: message.into(),
            failure,
        }
    }

    pub fn failure(&self) -> Option<&AccountFailureObservation> {
        self.failure.as_ref()
    }
}

#[derive(Debug, Clone)]
pub struct AccountUsageResult {
    pub quota: QuotaSnapshot,
    pub raw: Value,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AccountProbeRequest {
    pub model: String,
    pub instructions: String,
    pub input_text: String,
}

#[derive(Debug, Clone)]
pub enum AccountProbeEvent {
    Content(String),
    Complete,
    Failed(AccountGatewayError),
}

pub type AccountProbeEventStream = Pin<Box<dyn Stream<Item = AccountProbeEvent> + Send>>;

pub struct AccountProbeSession {
    pub request_payload: Value,
    pub events: AccountProbeEventStream,
}

#[async_trait]
pub trait AccountUpstreamGateway: Send + Sync + 'static {
    async fn fetch_usage(
        &self,
        context: AccountUpstreamContext,
    ) -> Result<AccountUsageResult, AccountGatewayError>;

    async fn probe_response(
        &self,
        context: AccountUpstreamContext,
        request: AccountProbeRequest,
    ) -> Result<AccountProbeSession, AccountGatewayError>;

    async fn evict_account_connections(&self, account_id: &str);
}
