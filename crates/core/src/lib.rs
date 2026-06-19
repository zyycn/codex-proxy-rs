#![deny(missing_docs)]

//! 核心领域层，承载纯业务模型、协议语义和策略。

pub mod accounts;
pub mod admin;
pub mod auth;
pub mod error;
pub mod events;
pub mod gateway;
pub mod models;
pub mod protocol;
pub mod serving;
pub mod usage;
