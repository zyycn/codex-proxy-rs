//! OpenAI 客户端协议 adapter。

pub mod auth;
pub mod error;
pub mod models;
pub mod responses;
pub mod router;
pub mod service;

pub use router::router;
pub use service::{
    ConnectionTask, DeliveryEvent, OpenAiApiState, OpenAiClientService, ResponseExecutionSession,
    ResponsesTransport, StartedResponse,
};
