#![deny(missing_docs)]

//! 平台基础设施层，承载配置、加密、身份、存储、日志和 JSON 原语。

pub mod config;
pub mod crypto;
pub mod identity;
pub mod json;
pub mod logging;
pub mod storage;
