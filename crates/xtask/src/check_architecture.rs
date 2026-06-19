//! 架构检查命令。

use crate::{repository_root, run_command, XtaskResult};

/// 运行架构测试。
pub fn run() -> XtaskResult {
    let root = repository_root()?;
    run_command("cargo", &["test", "--test", "architecture"], &root)
}
