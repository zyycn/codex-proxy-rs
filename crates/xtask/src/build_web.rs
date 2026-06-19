//! 前端构建命令。

use crate::{repository_root, run_command, XtaskResult};

/// 构建 web 前端资源。
pub fn run() -> XtaskResult {
    let web_dir = repository_root()?.join("web");
    run_command("pnpm", &["install", "--frozen-lockfile"], &web_dir)?;
    run_command("pnpm", &["build"], &web_dir)
}
