//! 有界解码插件归档，校验清单、资源路径与内容摘要

use std::{collections::BTreeMap, io::Read, sync::Arc};

use flate2::read::MultiGzDecoder;
use gateway_plugin_sdk::{Manifest, ManifestError, valid_package_path};
use sha2::{Digest as _, Sha256};

#[derive(Debug, Clone, Copy)]
pub struct PackageLimits {
    pub compressed_bytes: usize,
    pub expanded_bytes: usize,
    pub file_count: usize,
    pub manifest_bytes: usize,
}

impl Default for PackageLimits {
    fn default() -> Self {
        Self {
            compressed_bytes: 32 * 1024 * 1024,
            expanded_bytes: 128 * 1024 * 1024,
            file_count: 256,
            manifest_bytes: 64 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PackageError {
    #[error("plugin package exceeds configured limits")]
    Limit,
    #[error("plugin package archive is invalid")]
    Archive,
    #[error("plugin package digest does not match")]
    Digest,
    #[error("plugin package contains an unsafe or duplicate path")]
    Path,
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("plugin artifact cache is unavailable")]
    Cache,
}

/// 校验后的不可变字节；无需先把未可信压缩包解压到宿主文件系统
#[derive(Clone)]
pub struct ValidatedPackage {
    pub(super) manifest: Manifest,
    pub(super) digest: String,
    pub(super) archive: Arc<[u8]>,
    pub(super) files: BTreeMap<String, Vec<u8>>,
}

impl ValidatedPackage {
    /// 仅供已冻结资源声明读取校验后的字节，不暴露缓存路径或任意文件读取
    pub(crate) fn resource(&self, path: &str) -> Option<&[u8]> {
        self.manifest
            .resources
            .contains_key(path)
            .then(|| self.files.get(path).map(Vec::as_slice))
            .flatten()
    }

    pub fn read(
        archive: Arc<[u8]>,
        expected_sha256: Option<&str>,
        limits: PackageLimits,
    ) -> Result<Self, PackageError> {
        if archive.len() > limits.compressed_bytes {
            return Err(PackageError::Limit);
        }
        let digest = hex::encode(Sha256::digest(&archive));
        if expected_sha256.is_some_and(|expected| expected != digest) {
            return Err(PackageError::Digest);
        }

        // 先限制整个解压字节流，连 PAX 元数据、目录、padding 和尾部数据也纳入预算
        let expanded_limit =
            u64::try_from(limits.expanded_bytes).map_err(|_| PackageError::Limit)?;
        let mut expanded = Vec::new();
        MultiGzDecoder::new(archive.as_ref())
            .take(expanded_limit.saturating_add(1))
            .read_to_end(&mut expanded)
            .map_err(|_| PackageError::Archive)?;
        if expanded.len() > limits.expanded_bytes {
            return Err(PackageError::Limit);
        }
        let mut tar = tar::Archive::new(expanded.as_slice());
        let mut files = BTreeMap::new();
        let mut folded_paths = std::collections::BTreeSet::new();
        let mut total = 0usize;
        for entry in tar.entries().map_err(|_| PackageError::Archive)? {
            let mut entry = entry.map_err(|_| PackageError::Archive)?;
            if !entry.header().entry_type().is_file() {
                return Err(PackageError::Path);
            }
            let path = std::str::from_utf8(&entry.path_bytes())
                .map_err(|_| PackageError::Path)?
                .to_owned();
            if !valid_package_path(&path) || !folded_paths.insert(path.to_ascii_lowercase()) {
                return Err(PackageError::Path);
            }
            if files.len() > limits.file_count {
                return Err(PackageError::Limit);
            }
            let size = usize::try_from(entry.size()).map_err(|_| PackageError::Limit)?;
            total = total.checked_add(size).ok_or(PackageError::Limit)?;
            if total > limits.expanded_bytes
                || (path == "plugin.json" && size > limits.manifest_bytes)
            {
                return Err(PackageError::Limit);
            }
            let mut data = Vec::new();
            entry
                .read_to_end(&mut data)
                .map_err(|_| PackageError::Archive)?;
            if data.len() != size {
                return Err(PackageError::Archive);
            }
            files.insert(path, data);
        }
        let manifest_bytes = files.remove("plugin.json").ok_or(PackageError::Archive)?;
        let manifest: Manifest =
            serde_json::from_slice(&manifest_bytes).map_err(|_| PackageError::Archive)?;
        manifest.validate_structure()?;
        // 源清单可以省略 package，但归档安装入口必须只接受构建后的单平台包
        let package = manifest.package.as_ref().ok_or(ManifestError::Invalid)?;
        if files.len() != package.files.len() || files.len() > limits.file_count {
            return Err(PackageError::Archive);
        }
        for (path, expected) in &package.files {
            let data = files.get(path).ok_or(PackageError::Archive)?;
            if hex::encode(Sha256::digest(data)) != *expected {
                return Err(PackageError::Digest);
            }
        }
        super::icon::validate(&manifest, &files)?;
        Ok(Self {
            manifest,
            digest,
            archive,
            files,
        })
    }

    #[must_use]
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    #[must_use]
    pub fn archive(&self) -> Arc<[u8]> {
        Arc::clone(&self.archive)
    }
}
