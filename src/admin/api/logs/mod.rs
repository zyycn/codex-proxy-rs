use axum::http::StatusCode;

use crate::codex::events::service::LogServiceError;

use super::AdminError;

pub mod detail;
pub mod query;
pub mod state;

pub use detail::log_detail;
pub use query::logs;
pub use state::{clear_logs, logs_state, update_logs_state};

pub(super) fn log_service_error(error: LogServiceError, request_id: String) -> AdminError {
    match error {
        LogServiceError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Event log repository is not initialized",
            request_id,
        ),
        LogServiceError::List => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to list event logs",
            request_id,
        ),
        LogServiceError::Get => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to load event log",
            request_id,
        ),
        LogServiceError::Count => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to count event logs",
            request_id,
        ),
        LogServiceError::Clear => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to clear event logs",
            request_id,
        ),
        LogServiceError::Write => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to write event log",
            request_id,
        ),
        LogServiceError::InvalidCapacity => AdminError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            42201,
            "logsCapacity must be greater than 0",
            request_id,
        ),
    }
}
