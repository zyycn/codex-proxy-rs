//! 数据目录、installation ID 等路径辅助。

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use uuid::Uuid;

const INSTALLATION_ID_FILE_NAME: &str = "installation_id";

/// 返回本地数据目录。
pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("codex-proxy-rs")
        .join("data")
}

/// 确保本地数据目录存在。
pub fn ensure_data_dir() -> std::io::Result<PathBuf> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// 返回真实 Codex Desktop installation ID 文件路径。
pub fn codex_desktop_installation_id_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".codex").join(INSTALLATION_ID_FILE_NAME))
}

/// 返回本应用数据目录中的 installation ID 文件路径。
pub fn data_installation_id_path(data_dir: &Path) -> PathBuf {
    data_dir.join(INSTALLATION_ID_FILE_NAME)
}

/// 按读取优先级读取或生成 installation ID。
///
/// 顺序为真实 Codex Desktop 文件、本应用数据目录文件、生成并写入本应用数据目录。
/// 当 `data_dir` 为 `None` 且没有可读文件时，返回一个不会持久化的新 UUID。
pub fn load_or_create_installation_id(data_dir: Option<&Path>) -> io::Result<String> {
    let codex_path = codex_desktop_installation_id_path();
    let data_path = data_dir.map(data_installation_id_path);
    load_or_create_installation_id_from_paths(codex_path.as_deref(), data_path.as_deref())
}

/// 从显式文件路径按读取优先级读取或生成 installation ID。
///
/// 此函数用于测试和调用方已经完成路径解析的场景。
pub fn load_or_create_installation_id_from_paths(
    codex_desktop_path: Option<&Path>,
    data_path: Option<&Path>,
) -> io::Result<String> {
    if let Some(path) = codex_desktop_path {
        if let Some(id) = read_installation_id(path)? {
            return Ok(id);
        }
    }

    if let Some(path) = data_path {
        if let Some(id) = read_installation_id(path)? {
            return Ok(id);
        }

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
