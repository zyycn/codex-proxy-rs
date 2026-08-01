//! 受控暂存目录：权限校验、磁盘空间检查与原子改名。

use std::path::{Path, PathBuf};

use crate::{StoreError, StoreResult};

/// 单任务暂存归档上限（默认 64 GiB）。
pub const DEFAULT_MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
/// 暂存目录所需的最小剩余空间；与单归档上限解耦，避免小磁盘环境被 64 GiB 门槛误拒。
const MIN_STAGING_FREE_BYTES: u64 = 1024 * 1024 * 1024;

/// 备份暂存区。只负责文件系统；归档校验与上传由调用方负责。
#[derive(Debug, Clone)]
pub struct StagingArea {
    base_dir: PathBuf,
    max_archive_bytes: u64,
}

impl StagingArea {
    /// 创建暂存区并确保目录存在、权限为仅运行用户可读写（0700）。
    ///
    /// # Errors
    ///
    /// 目录创建或权限设置失败时返回 [`StoreError`]。
    pub fn open(base_dir: PathBuf, max_archive_bytes: u64) -> StoreResult<Self> {
        std::fs::create_dir_all(&base_dir).map_err(|_| unavailable("create staging directory"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&base_dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| unavailable("set staging directory permissions"))?;
        }
        let metadata = std::fs::metadata(&base_dir)
            .map_err(|_| unavailable("read staging directory metadata"))?;
        if !metadata.is_dir() {
            return Err(invalid("staging path is not a directory"));
        }
        Ok(Self {
            base_dir,
            max_archive_bytes,
        })
    }

    /// 未完成归档的暂存路径。
    #[must_use]
    pub fn partial_path(&self, backup_id: &str) -> PathBuf {
        self.base_dir.join(format!("{backup_id}.partial"))
    }

    /// 完成归档的暂存路径。
    #[must_use]
    pub fn final_path(&self, backup_id: &str) -> PathBuf {
        self.base_dir.join(format!("{backup_id}.dump"))
    }

    /// 单任务暂存归档上限。
    #[must_use]
    pub const fn max_archive_bytes(&self) -> u64 {
        self.max_archive_bytes
    }

    /// 检查剩余磁盘空间是否足够暂存一个归档。
    ///
    /// 只要求保留一个基本工作余量；单个归档的硬上限由
    /// [`Self::max_archive_bytes`] 在写入时兜底。
    ///
    /// # Errors
    ///
    /// 剩余空间不足或读取失败时返回 [`crate::StoreError`]。
    pub fn ensure_capacity(&self) -> StoreResult<()> {
        let free = fs2::available_space(&self.base_dir)
            .map_err(|_| unavailable("read staging free space"))?;
        if free < MIN_STAGING_FREE_BYTES {
            return Err(StoreError::InvalidData {
                entity: "backup staging",
                message: "staging disk space is below 1 GiB".to_owned(),
            });
        }
        Ok(())
    }

    /// 清理该备份在暂存区的全部文件；不存在视为成功。
    pub fn cleanup(&self, backup_id: &str) {
        let _ = std::fs::remove_file(self.partial_path(backup_id));
        let _ = std::fs::remove_file(self.final_path(backup_id));
    }

    /// 校验一个已完成归档的暂存路径归属本暂存区。
    #[must_use]
    pub fn owns(&self, path: &Path) -> bool {
        path.parent() == Some(self.base_dir.as_path())
    }
}

fn unavailable(operation: &'static str) -> StoreError {
    crate::postgres_unavailable(operation)
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: "backup staging",
        message: message.to_owned(),
    }
}
