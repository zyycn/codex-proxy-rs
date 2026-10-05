//! 有界、请求局部的诊断证据；不参与路由、重试或客户端协议决策

mod capture;
mod selection;
mod stream;
mod trace;

pub use capture::{body_fingerprint, diagnostic_headers, diagnostic_json};
pub use stream::{StreamCapture, StreamFormat};
pub use trace::TraceContext;
