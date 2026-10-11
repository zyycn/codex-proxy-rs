//! OAuth 刷新失败的受控观测投影，普通日志只记录分类和关联

use super::token_client::RefreshFailure;
use gateway_core::{
    account::ProviderAccount,
    diagnostics::{OperationalDiagnostics, OperationalFailure},
    error::{ErrorDetails, ErrorSource, OpaqueUpstreamValue, RawUpstreamError},
    identity::ProviderKind,
};

pub(crate) async fn record_refresh_failure(
    diagnostics: &dyn OperationalDiagnostics,
    account: &ProviderAccount,
    operation: &'static str,
    error: &RefreshFailure,
) {
    let account_id = account.id();
    let upstream = error.upstream();
    let mut failure = OperationalFailure::new(
        "oauth",
        operation,
        error.classification(),
        error.to_string(),
    );
    failure.provider_kind = Some(ProviderKind::new("openai").expect("static provider"));
    failure.account_id = Some(account_id.clone());
    failure.account_snapshot = Some(gateway_core::account::AccountHistorySnapshot::from_account(
        account,
    ));
    failure.upstream_status = upstream.map(|value| value.status());
    failure.upstream_code = upstream
        .and_then(|value| value.code())
        .map(|code| OpaqueUpstreamValue::new(code.to_owned()));
    let source = ErrorSource::new(RefreshDiagnostic(error.clone()));
    let body = upstream.map(|value| RawUpstreamError::new(value.body()));
    failure.details = ErrorDetails::capture(Some(&source), body.as_ref(), error.redacted());
    if diagnostics.record_failure(failure).await.is_err() {
        tracing::warn!(account_id = %account_id, operation, "OAuth diagnostic could not be recorded");
    }
}

#[derive(Debug)]
struct RefreshDiagnostic(RefreshFailure);
impl std::fmt::Display for RefreshDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0.message() {
            Some(message) => formatter.write_str(message),
            None => std::fmt::Display::fmt(&self.0, formatter),
        }
    }
}
impl std::error::Error for RefreshDiagnostic {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}
