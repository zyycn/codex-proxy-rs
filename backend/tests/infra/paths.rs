use std::{ffi::OsString, fs, sync::Mutex};

use codex_proxy_rs::infra::paths::{ensure_data_dir, load_or_create_installation_id};

const INSTALLATION_ID_FILE_NAME: &str = "installation_id";

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
fn load_or_create_installation_id_should_persist_under_data_dir_root() {
    let dir = tempfile::tempdir().unwrap();

    let installation_id =
        load_or_create_installation_id(Some(dir.path())).expect("installation id should generate");

    assert_eq!(
        fs::read_to_string(dir.path().join(INSTALLATION_ID_FILE_NAME)).unwrap(),
        installation_id
    );
}

#[test]
fn load_or_create_installation_id_should_seed_data_dir_from_codex_desktop_id() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let codex_path = home.path().join(".codex").join(INSTALLATION_ID_FILE_NAME);
    let desktop_id = "018f8f6b-1d7b-7b7c-b9c8-8c9f4c6d0e1a";
    fs::create_dir_all(codex_path.parent().unwrap()).unwrap();
    fs::write(&codex_path, desktop_id).unwrap();
    let _home = EnvVarGuard::set("HOME", home.path().as_os_str().to_owned());

    let installation_id =
        load_or_create_installation_id(Some(data.path())).expect("installation id should seed");

    assert_eq!(installation_id, desktop_id);
    assert_eq!(
        fs::read_to_string(data.path().join(INSTALLATION_ID_FILE_NAME)).unwrap(),
        desktop_id
    );
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
