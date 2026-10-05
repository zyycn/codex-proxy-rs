//! 验证网关基础命令无需加载配置，并拒绝非法或多余参数

use std::{fs, process::Command};

#[test]
fn basic_cli_options_do_not_load_configuration_and_reject_unexpected_arguments() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("deploy")).unwrap();
    fs::write(directory.path().join("deploy/config.yaml"), "invalid: [").unwrap();
    for flag in ["--help", "-h", "help", "--version", "-V"] {
        let output = Command::new(env!("CARGO_BIN_EXE_codex-proxy-rs"))
            .arg(flag)
            .current_dir(directory.path())
            .env_clear()
            .output()
            .unwrap();
        assert!(output.status.success(), "{flag}");
        assert!(!output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
    for args in [
        vec!["unknown"],
        vec!["--help", "extra"],
        vec!["serve", "extra"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_codex-proxy-rs"))
            .args(args)
            .current_dir(directory.path())
            .env_clear()
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("configuration"));
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_command_is_not_interpreted_as_serve() {
    use std::os::unix::ffi::OsStringExt as _;

    let output = Command::new(env!("CARGO_BIN_EXE_codex-proxy-rs"))
        .arg(std::ffi::OsString::from_vec(vec![0xff]))
        .env_clear()
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("command must be UTF-8"));
}
