//! 运行设置的纯值、校验与编译结果

pub(crate) mod compiled;
pub mod privacy;
mod values;
pub use values::{
    SettingsValues, client_min_versions, response_body_limit, validate_request_limits,
};

#[derive(Debug, thiserror::Error)]
#[error("request settings are invalid")]
pub struct InvalidSettings;
