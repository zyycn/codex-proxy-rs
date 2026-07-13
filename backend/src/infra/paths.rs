//! 数据目录与持久身份密钥路径辅助。

use std::{
    fs, io,
    io::Write,
    path::{Path, PathBuf},
};

use rand::Rng;

const IDENTITY_SECRET_FILE_NAME: &str = "identity_hmac_secret";

/// 确保本地数据目录存在。
pub fn ensure_data_dir(directory: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    let directory = directory.as_ref();
    std::fs::create_dir_all(directory)?;
    Ok(directory.to_path_buf())
}

/// 读取或创建账号身份隔离使用的 256-bit HMAC 密钥。
pub fn load_or_create_identity_secret(data_dir: &Path) -> io::Result<[u8; 32]> {
    let path = data_dir.join(IDENTITY_SECRET_FILE_NAME);
    match read_identity_secret(&path) {
        Ok(Some(secret)) => return Ok(secret),
        Ok(None) => {}
        Err(error) => return Err(error),
    }

    let mut secret = [0u8; 32];
    rand::rng().fill_bytes(&mut secret);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            file.write_all(hex::encode(secret).as_bytes())?;
            file.sync_all()?;
            Ok(secret)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => read_identity_secret(&path)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid identity secret")),
        Err(error) => Err(error),
    }
}

fn read_identity_secret(path: &Path) -> io::Result<Option<[u8; 32]>> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let decoded = hex::decode(raw.trim())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let secret = decoded.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "identity secret must be 32 bytes",
        )
    })?;
    Ok(Some(secret))
}
