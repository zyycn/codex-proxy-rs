//! 系统管理用例与更新前置检查的模块门面

mod preflight;
mod service;

pub(crate) use preflight::PluginSystemUpdatePreflight;
pub(crate) use service::DefaultSystemService;
pub use service::SystemService;
