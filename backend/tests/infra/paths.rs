use std::{fs, path::PathBuf, process::Command};

use codex_proxy_rs::infra::paths::{ensure_data_dir, load_or_create_identity_secret};

const IDENTITY_SECRET_FILE_NAME: &str = "identity_hmac_secret";
const DATA_DIR_CASE_ENV: &str = "CODEX_PROXY_TEST_DATA_DIR_CASE";

#[test]
fn data_dir_should_use_xdg_data_home_directly() {
    if std::env::var(DATA_DIR_CASE_ENV).as_deref() == Ok("child") {
        let expected = PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap());
        assert_eq!(ensure_data_dir().unwrap(), expected);
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("infra::paths::data_dir_should_use_xdg_data_home_directly")
        .arg("--nocapture")
        .env(DATA_DIR_CASE_ENV, "child")
        .env("XDG_DATA_HOME", dir.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "isolated data directory test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn identity_secret_should_be_stable_and_owner_only() {
    let dir = tempfile::tempdir().unwrap();

    let first = load_or_create_identity_secret(dir.path()).expect("secret should generate");
    let second = load_or_create_identity_secret(dir.path()).expect("secret should reload");

    assert_eq!(first, second);
    let path = dir.path().join(IDENTITY_SECRET_FILE_NAME);
    assert_eq!(fs::read_to_string(&path).unwrap(), hex::encode(first));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn identity_secret_should_fail_closed_when_existing_file_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(IDENTITY_SECRET_FILE_NAME), "not-a-secret").unwrap();

    let error = load_or_create_identity_secret(dir.path()).expect_err("invalid secret must fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}
