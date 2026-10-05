//! 插件运行时包校验、RPC、能力适配与发布代次的测试入口

mod adapter;
mod callback;
mod generation;
mod package;
#[cfg(unix)]
mod rpc;
mod support;
