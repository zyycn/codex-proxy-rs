//! OpenAI 客户端协议 adapter。

pub mod auth;
pub mod error;
pub mod images;
pub mod models;
mod provider_endpoint;
pub mod responses;
pub mod router;
pub mod search;
pub(crate) mod service;
