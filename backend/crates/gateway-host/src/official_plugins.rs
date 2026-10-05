//! 随宿主发行的官方插件文件只读入口

use std::{
    fs,
    io::Read as _,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use gateway_admin::ports::plugin_release::{
    OfficialPluginReleaseFiles, OfficialPluginReleaseReadError, OfficialPluginReleaseReadErrorKind,
};

const MANIFEST_FILE_NAME: &str = "plugin-release-manifest.json";
const MAXIMUM_MANIFEST_BYTES: usize = 256 * 1024;
const MAXIMUM_ARTIFACT_BYTES: usize = 32 * 1024 * 1024;

/// 目录与宿主二进制一同部署和替换；`sealed` 本身不被当作密码学签名
pub struct FileOfficialPluginRelease {
    directory: PathBuf,
}

impl FileOfficialPluginRelease {
    #[must_use]
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }
}

#[async_trait]
impl OfficialPluginReleaseFiles for FileOfficialPluginRelease {
    async fn manifest(&self) -> Result<Option<Arc<[u8]>>, OfficialPluginReleaseReadError> {
        let directory = self.directory.clone();
        tokio::task::spawn_blocking(move || {
            let Some(path) = release_file(&directory, MANIFEST_FILE_NAME, true)? else {
                return Ok(None);
            };
            read_bounded(&path, MAXIMUM_MANIFEST_BYTES).map(Some)
        })
        .await
        .map_err(|_| unavailable())?
    }

    async fn artifact(&self, file_name: &str) -> Result<Arc<[u8]>, OfficialPluginReleaseReadError> {
        if !valid_file_name(file_name) {
            return Err(invalid());
        }
        let directory = self.directory.clone();
        let file_name = file_name.to_owned();
        tokio::task::spawn_blocking(move || {
            let path = release_file(&directory, &file_name, false)?.ok_or_else(not_found)?;
            read_bounded(&path, MAXIMUM_ARTIFACT_BYTES)
        })
        .await
        .map_err(|_| unavailable())?
    }
}

fn release_file(
    directory: &Path,
    file_name: &str,
    manifest: bool,
) -> Result<Option<PathBuf>, OfficialPluginReleaseReadError> {
    let Some(parent) = directory.parent() else {
        return Err(invalid());
    };
    let parent_metadata = match fs::symlink_metadata(parent) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && manifest => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(not_found()),
        Err(_) => return Err(unavailable()),
    };
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        return Err(invalid());
    }
    let directory_metadata = match fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && manifest => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(not_found()),
        Err(_) => return Err(unavailable()),
    };
    if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
        return Err(invalid());
    }
    let path = directory.join(file_name);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && manifest => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(not_found()),
        Err(_) => return Err(unavailable()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(invalid());
    }
    Ok(Some(path))
}

fn read_bounded(path: &Path, maximum: usize) -> Result<Arc<[u8]>, OfficialPluginReleaseReadError> {
    let file = fs::File::open(path).map_err(|_| unavailable())?;
    let metadata = file.metadata().map_err(|_| unavailable())?;
    if !metadata.is_file() || metadata.len() > u64::try_from(maximum).unwrap_or(u64::MAX) {
        return Err(invalid());
    }
    let limit = u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1);
    let mut bytes = Vec::new();
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    if bytes.len() > maximum {
        return Err(invalid());
    }
    Ok(bytes.into())
}

fn valid_file_name(value: &str) -> bool {
    !value.is_empty()
        && Path::new(value).components().count() == 1
        && matches!(
            Path::new(value).components().next(),
            Some(Component::Normal(_))
        )
        && Path::new(value).file_name().and_then(|name| name.to_str()) == Some(value)
}

fn invalid() -> OfficialPluginReleaseReadError {
    OfficialPluginReleaseReadError::new(OfficialPluginReleaseReadErrorKind::Invalid)
}

fn not_found() -> OfficialPluginReleaseReadError {
    OfficialPluginReleaseReadError::new(OfficialPluginReleaseReadErrorKind::NotFound)
}

fn unavailable() -> OfficialPluginReleaseReadError {
    OfficialPluginReleaseReadError::new(OfficialPluginReleaseReadErrorKind::Unavailable)
}
