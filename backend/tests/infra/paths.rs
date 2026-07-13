use std::fs;

use codex_proxy_rs::infra::paths::{ensure_data_dir, load_or_create_identity_secret};

const IDENTITY_SECRET_FILE_NAME: &str = "identity_hmac_secret";
#[test]
fn data_dir_should_use_explicit_configured_directory() {
    let dir = tempfile::tempdir().unwrap();
    let configured = dir.path().join("configured-data");

    assert_eq!(ensure_data_dir(&configured).unwrap(), configured);
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
