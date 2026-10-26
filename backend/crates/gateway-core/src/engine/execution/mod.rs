//! 数据面执行入口与请求、作用域、会话的单一所有权

mod admission;
mod contract;
mod delivery;
pub use delivery::ResponseCommit;
mod failure;
pub use contract::{
    AuthenticatedClient, ClientApiKeyUsageSink, ClientAuthenticationError, ClientKeyVerifier,
    ClientTransport, ExecutionRequestMetadata, ExecutionService, ExecutionSession,
    PreparedExecutionRequest, PreparedRootExecution, StartExecution, StartProviderExecution,
    StartedExecution,
};
mod scope;
mod service;
pub use failure::gateway_error_from_engine;
pub use service::DefaultExecutionService;
mod probe_store;
mod session;

use std::time::Duration;
const DIAGNOSTIC_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const COORDINATION_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_NESTED_EXECUTIONS: usize = 16;
const MAX_CONCURRENT_NESTED_EXECUTIONS: usize = 4;
