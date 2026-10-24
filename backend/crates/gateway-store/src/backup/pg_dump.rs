//! `pg_dump` 子进程与本地暂存适配器
//!
//! 数据库密码只通过 `PGPASSWORD` 环境变量传给子进程，绝不进入命令行参数或日志
//! 导出以有界内存流式写入暂存文件并计算 SHA-256；取消时终止子进程并清理部分文件

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;

use gateway_admin::model::backup::{BackupError, code};
use gateway_admin::ports::backup::{DatabaseDumpPort, DumpArtifact, DumpRequest, StagedArtifact};

use super::{archive::ArchiveWriter, snapshot::BusinessSnapshot, staging::StagingArea};

/// `pg_dump` 导出适配器
pub struct PgDumpAdapter {
    staging: Arc<StagingArea>,
    pool: sqlx::PgPool,
    /// 不含密码的 PostgreSQL URL，用于 `--dbname`
    database_url: String,
    /// 数据库密码，仅通过 `PGPASSWORD` 注入
    database_password: String,
}

impl PgDumpAdapter {
    /// 组合暂存区与数据库连接事实
    #[must_use]
    pub fn new(
        staging: Arc<StagingArea>,
        database_url: impl Into<String>,
        database_password: impl Into<String>,
        pool: sqlx::PgPool,
    ) -> Self {
        Self {
            staging,
            pool,
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

        if request.cancellation.is_cancelled() {
            return Err(cancelled());
        }
        // 创建和最终改名不跨取消点，避免后台文件操作在清理之后重新留下文件
        let mut options = std::fs::OpenOptions::new();
        options.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options
            .open(&partial)
            .map_err(|_| pg_dump_failed("无法创建暂存文件"))?;
        let _partial_cleanup = PartialArchive(partial.clone());
        let snapshot = tokio::select! {
            biased;
            _ = request.cancellation.cancelled() => return Err(cancelled()),
            result = BusinessSnapshot::begin(&self.pool, &request.backup_id) => result?,
        };
        let mut command = Command::new("pg_dump");
        command
            .arg("--format=plain")
            .arg("--no-owner")
            .arg("--no-privileges")
            .arg("--dbname")
            .arg(&self.database_url)
            .env("PGPASSWORD", &self.database_password)
            .env("PGCONNECT_TIMEOUT", "10")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command.arg("--lock-wait-timeout=5s");
        snapshot.configure(&mut command);
        let mut child = command
            .spawn()
            .map_err(|_| pg_dump_failed("无法启动 pg_dump 进程"))?;
        let export = async {
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| pg_dump_failed("无法读取 pg_dump 输出"))?;
            let mut writer = ArchiveWriter::new(file, self.staging.max_archive_bytes());
            writer
                .write_all(snapshot.restore_preamble().as_bytes())
                .await
                .map_err(write_failed)?;
            let mut reader = tokio::io::BufReader::new(stdout);
            let mut buffer = vec![0_u8; 64 * 1024];
            loop {
                let n = reader
                    .read(&mut buffer)
                    .await
                    .map_err(|_| pg_dump_failed("读取 pg_dump 输出失败"))?;
                if n == 0 {
                    break;
                }
                writer.write_all(&buffer[..n]).await.map_err(write_failed)?;
            }
            let status = child
                .wait()
                .await
                .map_err(|_| pg_dump_failed("等待 pg_dump 退出失败"))?;
            if !status.success() {
                return Err(pg_dump_failed("pg_dump 非零退出"));
            }
            snapshot.append(&mut writer).await?;
            writer.finish().await.map_err(write_failed)
        };
        let result = tokio::select! {
            biased;
            _ = request.cancellation.cancelled() => Err(cancelled()),
            _ = tokio::time::sleep(std::time::Duration::from_secs(15 * 60)) => Err(pg_dump_failed("备份导出超过 15 分钟期限")),
            result = export => result,
        };
        let (size, sha256) = match result {
            Ok(result) => result,
            Err(error) => {
                // 正常错误和取消必须等到进程退出；任务被强制丢弃时由 kill_on_drop 接管
                if let Err(source) = child.kill().await {
                    tracing::warn!(backup_id = %request.backup_id, error = %source, "终止 pg_dump 失败");
                }
                return Err(error);
            }
        };
        std::fs::rename(&partial, &final_path).map_err(|_| pg_dump_failed("暂存归档改名失败"))?;
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
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(pg_dump_failed("读取暂存归档信息失败")),
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
        self.staging
            .cleanup(backup_id)
            .map_err(|_| pg_dump_failed("清理暂存归档失败"))
    }
}

// 只拥有未完成路径；完整归档必须留给恢复流程，不能随导出 future 析构删除
struct PartialArchive(PathBuf);

impl Drop for PartialArchive {
    fn drop(&mut self) {
        if let Err(error) = super::staging::remove_if_exists(&self.0) {
            tracing::warn!(error = %error, "清理未完成备份归档失败");
        }
    }
}

/// 以有界内存读取文件并计算 SHA-256
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

pub(super) fn pg_dump_failed(message: &'static str) -> BackupError {
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

pub(super) fn write_failed(error: std::io::Error) -> BackupError {
    if matches!(
        error.kind(),
        std::io::ErrorKind::FileTooLarge | std::io::ErrorKind::StorageFull
    ) {
        staging_space_exhausted()
    } else {
        pg_dump_failed("写入暂存归档失败")
    }
}
