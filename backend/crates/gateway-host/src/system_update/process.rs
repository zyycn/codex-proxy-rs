//! 宿主进程替换与容器内关闭语义。

use std::env;
use std::fs;
use std::process::{Command, Stdio};

use super::{OperationError, SystemUpdateConfig, internal};

const RESTART_DELAY_ENV: &str = "CPR_RESTART_DELAY_MS";
const DEFAULT_RESTART_DELAY_MS: u64 = 1_200;

pub(crate) fn spawn_replacement(config: &SystemUpdateConfig) -> Result<(), OperationError> {
    let executable = config.executable_path()?;
    let metadata = fs::metadata(&executable)
        .map_err(|error| internal(format!("failed to schedule replacement process: {error}")))?;
    if !metadata.is_file() {
        return Err(internal(
            "failed to schedule replacement process: executable is not a file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(internal(
                "failed to schedule replacement process: executable is not executable",
            ));
        }
    }

    let delay_ms = environment_value(RESTART_DELAY_ENV)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_RESTART_DELAY_MS);
    #[cfg(unix)]
    let mut command = {
        let delay = format!("{}.{:03}", delay_ms / 1_000, delay_ms % 1_000);
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("sleep \"$1\"; shift; exec \"$@\"")
            .arg("codex-proxy-rs-restart")
            .arg(delay)
            .arg(&executable)
            .args(env::args_os().skip(1));
        command
    };
    #[cfg(not(unix))]
    let mut command = {
        let mut command = Command::new(&executable);
        command
            .args(env::args_os().skip(1))
            .env(RESTART_DELAY_ENV, delay_ms.to_string());
        command
    };
    command.stdin(Stdio::null());
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| internal(format!("failed to schedule replacement process: {error}")))
}

pub(crate) fn environment_value(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}
