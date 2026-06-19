//! 发布打包命令。

use crate::{build_web, repository_root, run_command, XtaskResult};

/// 执行发布打包流程。
pub fn run() -> XtaskResult {
    let root = repository_root()?;
    run_command("cargo", &["fmt", "--all", "--", "--check"], &root)?;
    run_command("cargo", &["test", "--workspace", "--all-targets"], &root)?;
    run_command(
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
        &root,
    )?;
    build_web::run()
}
