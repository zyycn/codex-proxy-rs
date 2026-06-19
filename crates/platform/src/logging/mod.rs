//! 日志初始化与文件轮转。

mod rotation;

pub use rotation::{build_file_appender, init_tracing, LogError, LogGuard, RotationConfig};
