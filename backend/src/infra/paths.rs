//! 数据目录、installation ID 等路径辅助。

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use uuid::Uuid;

const INSTALLATION_ID_FILE_NAME: &str = "installation_id";

fn data_dir() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".runtime/data"))
}

/// 确保本地数据目录存在。
pub fn ensure_data_dir() -> std::io::Result<PathBuf> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn codex_desktop_installation_id_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".codex").join(INSTALLATION_ID_FILE_NAME))
}

fn data_installation_id_path(data_dir: &Path) -> PathBuf {
    data_dir.join(INSTALLATION_ID_FILE_NAME)
}

/// 按读取优先级读取或生成 installation ID。
///
/// 顺序为本应用数据目录文件、真实 Codex Desktop 文件、生成并写入本应用数据目录。
/// 当 `data_dir` 为 `None` 且没有可读文件时，返回一个不会持久化的新 UUID。
pub fn load_or_create_installation_id(data_dir: Option<&Path>) -> io::Result<String> {
    let codex_path = codex_desktop_installation_id_path();
    let data_path = data_dir.map(data_installation_id_path);
    load_or_create_installation_id_from_paths(codex_path.as_deref(), data_path.as_deref())
}

fn load_or_create_installation_id_from_paths(
    codex_desktop_path: Option<&Path>,
    data_path: Option<&Path>,
) -> io::Result<String> {
    if let Some(path) = data_path {
        if let Some(id) = read_installation_id(path)? {
            return Ok(id);
        }
    }

    if let Some(path) = codex_desktop_path {
        if let Some(id) = read_installation_id(path)? {
            if let Some(data_path) = data_path {
                persist_installation_id(data_path, &id)?;
            }
            return Ok(id);
        }
    }

    if let Some(path) = data_path {
        let generated = generate_installation_id();
        persist_installation_id(path, &generated)?;
        return Ok(generated);
    }

    Ok(generate_installation_id())
}

fn read_installation_id(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(parse_installation_id(&content)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn persist_installation_id(path: &Path, installation_id: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, installation_id)
}

fn parse_installation_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    Uuid::parse_str(trimmed).ok()?;
    Some(trimmed.to_string())
}

fn generate_installation_id() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn data_dir_should_use_xdg_data_home_directly() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("XDG_DATA_HOME");
        std::env::set_var("XDG_DATA_HOME", dir.path());

        let resolved = data_dir();

        restore_env("XDG_DATA_HOME", previous);
        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn load_or_create_installation_id_should_persist_under_data_dir_root() {
        let dir = tempfile::tempdir().unwrap();
        let data_path = dir.path().join(INSTALLATION_ID_FILE_NAME);

        let installation_id = load_or_create_installation_id_from_paths(None, Some(&data_path))
            .expect("installation id should be generated");

        assert_eq!(fs::read_to_string(data_path).unwrap(), installation_id);
    }

    #[test]
    fn load_or_create_installation_id_should_seed_data_dir_from_codex_desktop_id() {
        let dir = tempfile::tempdir().unwrap();
        let codex_path = dir.path().join(".codex").join(INSTALLATION_ID_FILE_NAME);
        let data_path = dir.path().join("data").join(INSTALLATION_ID_FILE_NAME);
        let desktop_id = "018f8f6b-1d7b-7b7c-b9c8-8c9f4c6d0e1a";
        persist_installation_id(&codex_path, desktop_id).unwrap();

        let installation_id =
            load_or_create_installation_id_from_paths(Some(&codex_path), Some(&data_path))
                .expect("installation id should be seeded");

        assert_eq!(installation_id, desktop_id);
        assert_eq!(fs::read_to_string(data_path).unwrap(), desktop_id);
    }

    fn restore_env(key: &str, value: Option<std::ffi::OsString>) {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }
}
