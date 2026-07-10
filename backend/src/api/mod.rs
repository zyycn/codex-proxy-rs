//! 入站 HTTP API 与静态资源。

pub mod admin;
pub mod assets;
pub mod client;
pub mod middleware;
pub mod router;

pub use router::AppState;
