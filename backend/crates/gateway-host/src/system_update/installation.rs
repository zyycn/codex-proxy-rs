//! 安装文件指纹用于区分本进程安装的候选与外部重新部署，历史版本号不代表磁盘事实

use std::fs;
use std::io::{self, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::swap::backup_path_for;
use super::{OperationError, SystemUpdateConfig, conflict, internal};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseFiles {
    binary: String,
    web: String,
    official_plugins: String,
}

impl ReleaseFiles {
    pub(crate) fn installed(config: &SystemUpdateConfig) -> Result<Self, OperationError> {
        Self::read(
            &config.executable_path()?,
            config.web_dist_dir()?,
            &config.official_plugins_dir()?,
        )
    }

    pub(crate) fn backup(config: &SystemUpdateConfig) -> Result<Self, OperationError> {
        Self::read(
            &backup_path_for(&config.executable_path()?),
            &backup_path_for(config.web_dist_dir()?),
            &backup_path_for(&config.official_plugins_dir()?),
        )
    }

    fn read(binary: &Path, web: &Path, plugins: &Path) -> Result<Self, OperationError> {
        if !web.join("index.html").is_file()
            || !plugins.join(super::OFFICIAL_PLUGIN_MANIFEST).is_file()
        {
            return Err(conflict("安装文件不完整，请核对部署后重试"));
        }
        let mut binary_hash = Sha256::new();
        hash_file(binary, &mut binary_hash).map_err(read_error)?;
        Ok(Self {
            binary: hex::encode(binary_hash.finalize()),
            web: hash_directory(web).map_err(read_error)?,
            official_plugins: hash_directory(plugins).map_err(read_error)?,
        })
    }
}

fn read_error(error: io::Error) -> OperationError {
    tracing::warn!(error = %error, "核对系统安装文件失败");
    internal("无法核对安装文件，请检查文件完整性与读取权限")
}

fn hash_directory(path: &Path) -> io::Result<String> {
    let mut hash = Sha256::new();
    hash_tree(path, &mut hash)?;
    Ok(hex::encode(hash.finalize()))
}

fn hash_tree(path: &Path, hash: &mut Sha256) -> io::Result<()> {
    let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        let name = name.as_encoded_bytes();
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name);
        if entry.file_type()?.is_dir() {
            hash.update(b"directory");
            hash_tree(&entry.path(), hash)?;
            hash.update(b"end-directory");
        } else {
            hash.update(b"file");
            hash_file(&entry.path(), hash)?;
        }
    }
    Ok(())
}

fn hash_file(path: &Path, hash: &mut Sha256) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "发行文件必须为普通文件",
        ));
    }
    hash.update(metadata.len().to_le_bytes());
    let mut file = fs::File::open(path)?;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            return Ok(());
        }
        hash.update(&buffer[..count]);
    }
}
