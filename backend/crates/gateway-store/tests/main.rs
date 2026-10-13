//! PostgreSQL、Redis 与备份存储的测试入口

mod backup;
mod bundle;
mod config;
mod lease_renewal;
mod postgres;
mod redis;
mod support;

use gateway_store::StoreResult;

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
