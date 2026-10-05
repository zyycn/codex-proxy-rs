//! 二进制与 Web 制品的交易式替换、跨文件系统移动与回滚

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::archive::ExtractedRelease;
use super::{OperationError, SystemUpdateConfig, conflict, internal};

pub(crate) fn replace_release_files(
    executable: &Path,
    web_dist: &Path,
    extracted: ExtractedRelease,
) -> Result<(), OperationError> {
    let official_plugins = official_plugins_dir(executable)?;
    if !official_plugins.exists() {
        fs::create_dir_all(&official_plugins).map_err(|error| {
            internal(format!(
                "failed to prepare official plugin rollback directory: {error}"
            ))
        })?;
    }
    let official_backup = backup_path_for(&official_plugins);
    replace_dir(
        &official_plugins,
        &official_backup,
        &extracted.official_plugins_dir,
        "official plugin assets",
    )?;
    if let Err(error) = protect_official_plugins(&official_plugins) {
        let mut rollback = Vec::new();
        collect_rollback_error(
            &mut rollback,
            "restore official plugin assets",
            restore_dir(&official_plugins, &official_backup),
        );
        return Err(error_with_rollback(
            "failed to protect official plugin assets",
            error,
            rollback,
        ));
    }

    let web_backup = backup_path_for(web_dist);
    let web_replaced = match extracted.web_dist_dir {
        Some(new_web) => {
            if let Err(error) = replace_dir(web_dist, &web_backup, &new_web, "web assets") {
                let mut rollback = Vec::new();
                collect_rollback_error(
                    &mut rollback,
                    "restore official plugin assets",
                    restore_dir(&official_plugins, &official_backup),
                );
                return Err(error_with_rollback(
                    "failed to replace web assets",
                    error,
                    rollback,
                ));
            }
            true
        }
        None => false,
    };

    let binary_backup = backup_path_for(executable);
    if binary_backup.exists()
        && let Err(error) = fs::remove_file(&binary_backup)
    {
        let mut rollback = Vec::new();
        if web_replaced {
            collect_rollback_error(
                &mut rollback,
                "restore web assets",
                restore_dir(web_dist, &web_backup),
            );
        }
        collect_rollback_error(
            &mut rollback,
            "restore official plugin assets",
            restore_dir(&official_plugins, &official_backup),
        );
        return Err(error_with_rollback(
            "failed to remove old binary backup",
            error,
            rollback,
        ));
    }
    if let Err(error) = move_file(executable, &binary_backup) {
        let mut rollback = Vec::new();
        if web_replaced {
            collect_rollback_error(
                &mut rollback,
                "restore web assets",
                restore_dir(web_dist, &web_backup),
            );
        }
        collect_rollback_error(
            &mut rollback,
            "restore official plugin assets",
            restore_dir(&official_plugins, &official_backup),
        );
        return Err(error_with_rollback("binary backup failed", error, rollback));
    }
    if let Err(error) = move_file(&extracted.binary_path, executable) {
        let mut rollback = Vec::new();
        if executable.exists() {
            collect_rollback_error(
                &mut rollback,
                "remove partial replacement binary",
                fs::remove_file(executable),
            );
        }
        collect_rollback_error(
            &mut rollback,
            "restore previous binary",
            move_file(&binary_backup, executable),
        );
        if web_replaced {
            collect_rollback_error(
                &mut rollback,
                "restore web assets",
                restore_dir(web_dist, &web_backup),
            );
        }
        collect_rollback_error(
            &mut rollback,
            "restore official plugin assets",
            restore_dir(&official_plugins, &official_backup),
        );
        return Err(error_with_rollback(
            "binary replace failed",
            error,
            rollback,
        ));
    }
    Ok(())
}

pub(crate) fn rollback_release(config: &SystemUpdateConfig) -> Result<(), OperationError> {
    let web_dist = config.web_dist_dir()?;
    let executable = config.executable_path()?;
    let binary_backup = backup_path_for(&executable);
    if !binary_backup.exists() {
        return Err(conflict("no binary backup found for rollback"));
    }
    swap_file(&executable, &binary_backup)
        .map_err(|error| internal(format!("binary rollback failed: {error}")))?;

    let web_backup = backup_path_for(web_dist);
    if web_backup.exists()
        && let Err(error) = swap_dir(web_dist, &web_backup)
    {
        let mut rollback = Vec::new();
        collect_rollback_error(
            &mut rollback,
            "restore binary after web rollback failure",
            swap_file(&executable, &binary_backup),
        );
        return Err(error_with_rollback("web rollback failed", error, rollback));
    }
    let official_plugins = config.official_plugins_dir()?;
    let official_backup = backup_path_for(&official_plugins);
    if official_backup.exists()
        && let Err(error) = swap_dir(&official_plugins, &official_backup)
    {
        let mut rollback = Vec::new();
        if web_backup.exists() {
            collect_rollback_error(
                &mut rollback,
                "restore web assets after official plugin rollback failure",
                swap_dir(web_dist, &web_backup),
            );
        }
        collect_rollback_error(
            &mut rollback,
            "restore binary after official plugin rollback failure",
            swap_file(&executable, &binary_backup),
        );
        return Err(error_with_rollback(
            "official plugin rollback failed",
            error,
            rollback,
        ));
    }
    Ok(())
}

pub(crate) fn rollback_official_plugins_dir(
    config: &SystemUpdateConfig,
) -> Result<PathBuf, OperationError> {
    config
        .official_plugins_dir()
        .map(|directory| backup_path_for(&directory))
}

fn official_plugins_dir(executable: &Path) -> Result<PathBuf, OperationError> {
    executable
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .map(|path| path.join("plugins").join("official"))
        .ok_or_else(|| internal("failed to resolve official plugin release directory"))
}

fn replace_dir(
    current: &Path,
    backup: &Path,
    replacement: &Path,
    label: &'static str,
) -> Result<(), OperationError> {
    if backup.exists() {
        remove_dir_all(backup)
            .map_err(|error| internal(format!("failed to remove old {label} backup: {error}")))?;
    }
    if current.exists() {
        move_dir(current, backup)
            .map_err(|error| internal(format!("failed to backup {label}: {error}")))?;
    }
    if let Err(error) = move_dir(replacement, current) {
        let mut rollback = Vec::new();
        if backup.exists() {
            collect_rollback_error(
                &mut rollback,
                "restore previous directory contents",
                restore_dir(current, backup),
            );
        }
        return Err(error_with_rollback(
            "failed to replace directory contents",
            error,
            rollback,
        ));
    }
    Ok(())
}

fn swap_file(current: &Path, backup: &Path) -> io::Result<()> {
    if !current.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("current binary is missing: {}", current.display()),
        ));
    }
    let swap = swap_path_for(current);
    if swap.exists() {
        fs::remove_file(&swap)?;
    }
    move_file(current, &swap)?;
    if let Err(error) = move_file(backup, current) {
        let _ = move_file(&swap, current);
        return Err(error);
    }
    if let Err(error) = move_file(&swap, backup) {
        let _ = move_file(current, &swap);
        let _ = move_file(backup, current);
        let _ = move_file(&swap, backup);
        return Err(error);
    }
    Ok(())
}

fn swap_dir(current: &Path, backup: &Path) -> io::Result<()> {
    if !current.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("current web directory is missing: {}", current.display()),
        ));
    }
    let swap = swap_path_for(current);
    if swap.exists() {
        remove_dir_all(&swap)?;
    }
    move_dir(current, &swap)?;
    if let Err(error) = move_dir(backup, current) {
        let _ = move_dir(&swap, current);
        return Err(error);
    }
    if let Err(error) = move_dir(&swap, backup) {
        let _ = move_dir(current, &swap);
        let _ = move_dir(backup, current);
        let _ = move_dir(&swap, backup);
        return Err(error);
    }
    Ok(())
}

fn restore_dir(current: &Path, backup: &Path) -> io::Result<()> {
    if current.exists() {
        remove_dir_all(current)?;
    }
    if backup.exists() {
        move_dir(backup, current)?;
    }
    Ok(())
}

fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            if let Err(copy_error) = fs::copy(from, to) {
                let _ = fs::remove_file(to);
                return Err(copy_error);
            }
            fs::remove_file(from)
        }
        Err(error) => Err(error),
    }
}

fn move_dir(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            if let Err(copy_error) = copy_dir_all(from, to) {
                let _ = remove_dir_all(to);
                return Err(copy_error);
            }
            remove_dir_all(from)
        }
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!("directory rename failed: {error}"),
        )),
    }
}

#[cfg(unix)]
fn protect_official_plugins(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o555))
}

#[cfg(not(unix))]
fn protect_official_plugins(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn remove_dir_all(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    make_directories_writable(path)?;
    fs::remove_dir_all(path)
}

#[cfg(unix)]
fn make_directories_writable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "release directory contains an unsupported file type",
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            make_directories_writable(&entry.path())?;
        } else if !file_type.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "release directory contains an unsupported file type",
            ));
        }
    }
    Ok(())
}

fn copy_dir_all(from: &Path, to: &Path) -> io::Result<()> {
    let permissions = fs::metadata(from)?.permissions();
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = to.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &target)?;
            fs::set_permissions(&target, entry.metadata()?.permissions())?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("unsupported file type in {}", entry.path().display()),
            ));
        }
    }
    fs::set_permissions(to, permissions)?;
    Ok(())
}

pub(crate) fn backup_path_for(path: &Path) -> PathBuf {
    let mut backup = path.as_os_str().to_os_string();
    backup.push(".backup");
    PathBuf::from(backup)
}

fn swap_path_for(path: &Path) -> PathBuf {
    let mut swap = path.as_os_str().to_os_string();
    swap.push(".rollback-swap");
    PathBuf::from(swap)
}

fn collect_rollback_error(errors: &mut Vec<String>, action: &'static str, result: io::Result<()>) {
    if let Err(error) = result {
        errors.push(format!("{action}: {error}"));
    }
}

fn error_with_rollback(
    context: &'static str,
    error: impl fmt::Display,
    rollback_errors: Vec<String>,
) -> OperationError {
    if rollback_errors.is_empty() {
        return internal(format!("{context}: {error}"));
    }
    internal(format!(
        "{context}: {error}; rollback failed: {}",
        rollback_errors.join("; ")
    ))
}
