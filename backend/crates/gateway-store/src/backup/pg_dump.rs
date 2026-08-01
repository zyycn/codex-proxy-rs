//! `pg_dump` 子进程与本地暂存适配器。
//!
//! 数据库密码只通过 `PGPASSWORD` 环境变量传给子进程，绝不进入命令行参数或日志。
//! 导出以有界内存流式写入暂存文件并计算 SHA-256；取消时终止子进程并清理部分文件。

use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;

use gateway_admin::model::backup::{BackupError, code};
use gateway_admin::ports::backup::{DatabaseDumpPort, DumpArtifact, DumpRequest, StagedArtifact};

use super::staging::StagingArea;

/// `pg_dump` 导出适配器。
pub struct PgDumpAdapter {
    staging: Arc<StagingArea>,
    /// 不含密码的 PostgreSQL URL，用于 `--dbname`。
    database_url: String,
    /// 数据库密码，仅通过 `PGPASSWORD` 注入。
    database_password: String,
}

impl PgDumpAdapter {
    /// 组合暂存区与数据库连接事实。
    #[must_use]
    pub fn new(
        staging: Arc<StagingArea>,
        database_url: impl Into<String>,
        database_password: impl Into<String>,
    ) -> Self {
        Self {
            staging,
            database_url: database_url.into(),
            database_password: database_password.into(),
        }
    }
}

#[async_trait]
impl DatabaseDumpPort for PgDumpAdapter {
    async fn dump(&self, request: DumpRequest) -> Result<DumpArtifact, BackupError> {
        self.staging
            .ensure_capacity()
            .map_err(|_| staging_space_exhausted())?;
        let partial = self.staging.partial_path(&request.backup_id);
        let final_path = self.staging.final_path(&request.backup_id);

        let mut child = Command::new("pg_dump")
            .arg("--format=custom")
            .arg("--no-owner")
            .arg("--no-privileges")
            .arg("--dbname")
            .arg(&self.database_url)
            .env("PGPASSWORD", &self.database_password)
            .env("PGCONNECT_TIMEOUT", "10")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| pg_dump_failed("无法启动 pg_dump 进程"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| pg_dump_failed("无法读取 pg_dump 输出"))?;

        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&partial)
            .await
            .map_err(|_| pg_dump_failed("无法创建暂存文件"))?;
        let mut writer = tokio::io::BufWriter::new(file);
        let mut reader = tokio::io::BufReader::new(stdout);
        let mut hasher = Sha256::new();
        let mut size: u64 = 0;

        let copy = async {
            let mut buffer = vec![0_u8; 64 * 1024];
            loop {
                let n = reader
                    .read(&mut buffer)
                    .await
                    .map_err(|_| pg_dump_failed("读取 pg_dump 输出失败"))?;
                if n == 0 {
                    break;
                }
                size = size.saturating_add(n as u64);
                if size > self.staging.max_archive_bytes() {
                    return Err(staging_space_exhausted());
                }
                hasher.update(&buffer[..n]);
                writer
                    .write_all(&buffer[..n])
                    .await
                    .map_err(|_| pg_dump_failed("写入暂存文件失败"))?;
            }
            writer
                .flush()
                .await
                .map_err(|_| pg_dump_failed("刷新暂存文件失败"))?;
            Ok(())
        };

        let copy_result = tokio::select! {
            result = copy => result,
            _ = request.cancellation.cancelled() => {
                let _ = child.kill().await;
                Err(cancelled())
            }
        };

        if let Err(error) = copy_result {
            let _ = child.kill().await;
            let _ = child.wait().await;
            self.staging.cleanup(&request.backup_id);
            return Err(error);
        }

        let status = child
            .wait()
            .await
            .map_err(|_| pg_dump_failed("等待 pg_dump 退出失败"))?;
        if !status.success() {
            self.staging.cleanup(&request.backup_id);
            return Err(pg_dump_failed("pg_dump 非零退出"));
        }

        tokio::fs::rename(&partial, &final_path)
            .await
            .map_err(|_| pg_dump_failed("暂存归档改名失败"))?;
        let sha256 = hex::encode(hasher.finalize());
        Ok(DumpArtifact {
            path: final_path,
            size_bytes: size,
            sha256,
        })
    }

    async fn inspect_staging(
        &self,
        backup_id: &str,
    ) -> Result<Option<StagedArtifact>, BackupError> {
        let path = self.staging.final_path(backup_id);
        let metadata = match tokio::fs::metadata(&path).await {
            Ok(metadata) => metadata,
            Err(_) => return Ok(None),
        };
        if !metadata.is_file() {
            return Ok(None);
        }
        let size_bytes = metadata.len();
        let sha256 = hash_file(&path).await?;
        Ok(Some(StagedArtifact {
            path,
            size_bytes,
            sha256,
        }))
    }

    async fn cleanup_staging(&self, backup_id: &str) -> Result<(), BackupError> {
        self.staging.cleanup(backup_id);
        Ok(())
    }
}

/// 以有界内存读取文件并计算 SHA-256。
async fn hash_file(path: &std::path::Path) -> Result<String, BackupError> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| pg_dump_failed("打开暂存归档失败"))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buffer)
            .await
            .map_err(|_| pg_dump_failed("读取暂存归档失败"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn pg_dump_failed(message: &'static str) -> BackupError {
    BackupError::new(code::PG_DUMP_FAILED, message.to_owned())
}

fn staging_space_exhausted() -> BackupError {
    BackupError::new(
        code::STAGING_SPACE_EXHAUSTED,
        "暂存磁盘空间不足或归档超过单任务上限".to_owned(),
    )
}

fn cancelled() -> BackupError {
    BackupError::new(code::CANCELLED, "备份导出已取消".to_owned())
}
