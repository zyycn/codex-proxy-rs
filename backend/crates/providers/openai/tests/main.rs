//! OpenAI Provider、凭据与上游传输的测试入口

mod admin;
#[path = "../src/billing/mod.rs"]
#[allow(dead_code, unused_imports)]
mod billing;
#[path = "billing/mod.rs"]
mod billing_tests;
mod compat;
mod config;
mod credential;
mod jitter;
mod profile;
mod provider;
mod support;
mod transport;

#[derive(Default)]
struct RecordingDiagnostics(std::sync::Mutex<Vec<gateway_core::diagnostics::OperationalFailure>>);
#[async_trait::async_trait]
impl gateway_core::diagnostics::OperationalDiagnostics for RecordingDiagnostics {
    async fn record_failure(
        &self,
        failure: gateway_core::diagnostics::OperationalFailure,
    ) -> Result<(), gateway_core::error::StoreError> {
        self.0.lock().unwrap().push(failure);
        Ok(())
    }
}
