//! 二进制与 Web 制品的交易式替换、跨文件系统移动与回滚。

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
    let web_backup = backup_path_for(web_dist);
    let web_replaced = if let Some(new_web) = extracted.web_dist_dir {
        replace_dir(web_dist, &web_backup, &new_web)?;
        true
    } else {
        false
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
        return Err(error_with_rollback(
            "binary replace failed",
            error,
            rollback,
        ));
    }
    Ok(())
}

pub(crate) fn rollback_release(config: &SystemUpdateConfig) -> Result<(), OperationError> {
    let executable = config.executable_path()?;
    let binary_backup = backup_path_for(&executable);
    if !binary_backup.exists() {
        return Err(conflict("no binary backup found for rollback"));
    }
    swap_file(&executable, &binary_backup)
        .map_err(|error| internal(format!("binary rollback failed: {error}")))?;

    let web_backup = backup_path_for(&config.web_dist_dir);
    if web_backup.exists()
        && let Err(error) = swap_dir(&config.web_dist_dir, &web_backup)
    {
        let mut rollback = Vec::new();
        collect_rollback_error(
            &mut rollback,
            "restore binary after web rollback failure",
            swap_file(&executable, &binary_backup),
        );
        return Err(error_with_rollback("web rollback failed", error, rollback));
    }
    Ok(())
}

fn replace_dir(current: &Path, backup: &Path, replacement: &Path) -> Result<(), OperationError> {
    if backup.exists() {
        fs::remove_dir_all(backup)
            .map_err(|error| internal(format!("failed to remove old web backup: {error}")))?;
    }
    if current.exists() {
        move_dir(current, backup)
            .map_err(|error| internal(format!("failed to backup web assets: {error}")))?;
    }
    if let Err(error) = move_dir(replacement, current) {
        let mut rollback = Vec::new();
        if backup.exists() {
            collect_rollback_error(
                &mut rollback,
                "restore previous web assets",
                restore_dir(current, backup),
            );
        }
        return Err(error_with_rollback(
            "failed to replace web assets",
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
        fs::remove_dir_all(&swap)?;
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
        fs::remove_dir_all(current)?;
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
                let _ = fs::remove_dir_all(to);
                return Err(copy_error);
            }
            fs::remove_dir_all(from)
        }
        Err(error) => Err(error),
    }
}

fn copy_dir_all(from: &Path, to: &Path) -> io::Result<()> {
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
    Ok(())
}

fn backup_path_for(path: &Path) -> PathBuf {
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
