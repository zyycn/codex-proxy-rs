mod loader;
mod types;

pub use types::*;

pub type ConfigResult<T> = Result<T, ::config::ConfigError>;
