//! Installation ID 管理
//!
//! 稳定的设备级 UUID，作为 `x-codex-installation-id` header 和 `client_metadata` 发送到上游。
//! 用于路由/亲和性提示，使上游可以将客户端固定到同一后端实例以保持提示缓存温暖。
//!
//! 查找顺序：
//! 1. `~/.codex/installation_id`（兼容真实 Codex Desktop）
//! 2. `<database_dir>/installation_id`（之前持久化的）
//! 3. 生成新 UUID 并持久化到 `<database_dir>/installation_id`

use std::{
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use uuid::Uuid;

static INSTALLATION_ID: OnceLock<String> = OnceLock::new();

/// 获取 installation ID（首次调用后缓存）
pub fn get_installation_id(database_path: Option<&str>) -> String {
    INSTALLATION_ID
        .get_or_init(|| resolve_installation_id(database_path))
        .clone()
}

fn resolve_installation_id(database_path: Option<&str>) -> String {
    // 优先尝试 ~/.codex/installation_id（真实 Codex Desktop 位置）
    if let Some(home) = dirs::home_dir() {
        let codex_home = home.join(".codex").join("installation_id");
        if let Some(id) = read_uuid_file(&codex_home) {
            return id;
        }
    }

    // 从数据库路径推导数据目录（例如 "data/proxy.db" -> "data/"）
    let data_dir = database_path
        .and_then(|path| Path::new(path).parent())
        .map(|p| p.to_path_buf());

    if let Some(dir) = data_dir {
        let our_file = dir.join("installation_id");
        if let Some(id) = read_uuid_file(&our_file) {
            return id;
        }

        // 生成并持久化新 UUID
        let generated = Uuid::new_v4().to_string();
        if let Err(e) = persist_uuid(&our_file, &generated) {
            tracing::warn!("Failed to persist installation_id to {:?}: {}", our_file, e);
        }
        return generated;
    }

    // 降级：生成不持久化
    Uuid::new_v4().to_string()
}

fn read_uuid_file(path: &PathBuf) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    let trimmed = content.trim();

    // 验证 UUID 格式
    Uuid::parse_str(trimmed).ok()?;

    Some(trimmed.to_string())
}

fn persist_uuid(path: &PathBuf, uuid: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, uuid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_generate_and_persist() {
        let temp = TempDir::new().unwrap();
        let db_path = temp.path().join("proxy.db");
        let db_path_str = db_path.to_str().unwrap();

        let id1 = resolve_installation_id(Some(db_path_str));
        assert!(Uuid::parse_str(&id1).is_ok());

        let id2 = resolve_installation_id(Some(db_path_str));
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_read_from_codex_home() {
        let temp = TempDir::new().unwrap();
        let codex_dir = temp.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();

        let expected_id = Uuid::new_v4().to_string();
        fs::write(codex_dir.join("installation_id"), &expected_id).unwrap();

        let read_id = read_uuid_file(&codex_dir.join("installation_id"));
        assert_eq!(read_id, Some(expected_id));
    }
}
