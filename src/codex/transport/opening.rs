//! WebSocket 打开握手。

use std::{
    io,
    path::{Path, PathBuf},
};

pub use crate::codex::protocol::websocket::{OpeningAuditSnapshot, WebSocketAuditArtifact};
use chrono::Utc;
use uuid::Uuid;

/// WebSocket audit artifact 输出目录环境变量。
pub const WS_AUDIT_DIR_ENV: &str = "CODEX_PROXY_WS_AUDIT_DIR";

/// 构造握手审计快照。
pub fn opening_audit_snapshot(header_order: Vec<String>) -> OpeningAuditSnapshot {
    OpeningAuditSnapshot {
        header_order,
        ..OpeningAuditSnapshot::default()
    }
}

/// 显式写入 WebSocket audit artifact。
pub async fn write_websocket_audit_artifact_for_dir(
    dir: Option<&Path>,
    artifact: &WebSocketAuditArtifact,
) -> io::Result<Option<PathBuf>> {
    let Some(dir) = dir.filter(|dir| !dir.as_os_str().is_empty()) else {
        return Ok(None);
    };

    tokio::fs::create_dir_all(dir).await?;
    let path = dir.join(websocket_audit_file_name());
    let body = serde_json::to_vec_pretty(artifact).map_err(io::Error::other)?;
    tokio::fs::write(&path, body).await?;
    Ok(Some(path))
}

/// 按环境变量配置写入 WebSocket audit artifact。
pub async fn write_websocket_audit_artifact_from_env(
    artifact: &WebSocketAuditArtifact,
) -> io::Result<Option<PathBuf>> {
    let Some(dir) = std::env::var_os(WS_AUDIT_DIR_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    else {
        return Ok(None);
    };

    write_websocket_audit_artifact_for_dir(Some(&dir), artifact).await
}

fn websocket_audit_file_name() -> String {
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    format!("codex-ws-audit-{timestamp}-{}.json", Uuid::new_v4())
}
