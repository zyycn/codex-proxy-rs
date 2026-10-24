//! 执行层的请求、HTTP、WebSocket 与公开服务中间件合同

mod contract;
pub mod http;
pub mod outbound;
mod plan;
pub mod service;
pub mod websocket;

pub use contract::{
    MiddlewareAuthority, MiddlewareBody, MiddlewareCapabilityDeclaration, MiddlewareContext,
    MiddlewareError, MiddlewareFrame, MiddlewareFrameEnvelope, MiddlewareFraming, MiddlewareHeader,
    MiddlewareMount, MiddlewareNext, MiddlewareRequest, MiddlewareResponse,
    MiddlewareResponseEnvelope, MiddlewareTarget,
};
pub use plan::{FrozenMiddlewarePlan, MiddlewarePlan};
