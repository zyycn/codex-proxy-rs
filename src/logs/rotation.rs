use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RotationConfig {
    pub directory: PathBuf,
    pub max_file_bytes: u64,
    pub retention_days: u64,
}

impl RotationConfig {
    pub fn new(directory: impl AsRef<Path>, max_file_bytes: u64, retention_days: u64) -> Self {
        Self {
            directory: directory.as_ref().to_path_buf(),
            max_file_bytes,
            retention_days,
        }
    }
}
