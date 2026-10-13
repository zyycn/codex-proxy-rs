//! 版本、安全自更新、回滚与进程重启的 Host-owned 实现入口

mod archive;
mod config;
mod download;
mod error;
mod events;
mod installation;
mod operations;
mod process;
mod release;
mod state;
mod swap;
mod swap_paths;

pub use config::SystemUpdateConfig;
pub use operations::ProcessSystemOperations;
pub use release::validate_download_url;

use error::{OperationError, conflict, internal, invalid, upstream};
