use std::{ffi::OsString, fs, sync::Mutex};

use codex_proxy_rs::infra::paths::{ensure_data_dir, load_or_create_identity_secret};

const IDENTITY_SECRET_FILE_NAME: &str = "identity_hmac_secret";

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn data_dir_should_use_xdg_data_home_directly() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _xdg_data_home = EnvVarGuard::set("XDG_DATA_HOME", dir.path().as_os_str().to_owned());

    let resolved = ensure_data_dir().unwrap();

    assert_eq!(resolved, dir.path());
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

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: OsString) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}
