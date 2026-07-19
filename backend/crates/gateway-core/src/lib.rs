//! 多平台 AI 网关的数据面核心。
//!
//! 本 crate 只描述协议与 Provider 无关的业务语义。HTTP、数据库、Redis、
//! 具体客户端协议和具体 Provider 都通过外层 adapter 接入。

#![forbid(unsafe_code)]

pub mod accounting;
pub mod engine;
pub mod error;
pub mod event;
pub mod operation;
pub mod policy;
pub mod routing;
