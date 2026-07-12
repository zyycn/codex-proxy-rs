//! 自更新归档解压、文件替换与回滚。

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

use flate2::read::GzDecoder;
use tar::Archive;

use crate::update::types::UpdateError;

use super::service::{APP_BINARY_NAME, SystemUpdateConfig, internal_error, internal_error_with};

#[derive(Debug)]
pub(super) struct ExtractedRelease {
    binary_path: PathBuf,
    web_dist_dir: Option<PathBuf>,
}

pub(super) async fn rollback_release_update(
    config: &SystemUpdateConfig,
) -> Result<(), UpdateError> {
    let exe_path = config.executable_path()?;
    let backup_path = backup_path_for(&exe_path);
    if !backup_path.exists() {
        return Err(UpdateError::conflict("No binary backup found for rollback"));
    }
    fs::rename(&backup_path, &exe_path).map_err(internal_error_with("Binary rollback failed"))?;

    let web_backup = backup_path_for(&config.web_dist_dir);
    if web_backup.exists() {
        if config.web_dist_dir.exists() {
            fs::remove_dir_all(&config.web_dist_dir)
                .map_err(internal_error_with("Web rollback cleanup failed"))?;
        }
        fs::rename(&web_backup, &config.web_dist_dir)
            .map_err(internal_error_with("Web rollback failed"))?;
    }
    Ok(())
}

pub(super) fn extract_release_archive(
    archive_path: &Path,
    temp_dir: &Path,
) -> Result<ExtractedRelease, UpdateError> {
    let file =
        fs::File::open(archive_path).map_err(internal_error_with("Failed to open archive"))?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    let extract_dir = temp_dir.join("extracted");
    fs::create_dir_all(&extract_dir)
        .map_err(internal_error_with("Failed to create extract dir"))?;
    let binary_path = temp_dir.join(APP_BINARY_NAME);
    let web_dist_dir = temp_dir.join("web-dist");
    let mut found_binary = false;
    let mut found_web = false;

    for entry in archive
        .entries()
        .map_err(internal_error_with("Failed to read archive"))?
    {
        let mut entry = entry.map_err(internal_error_with("Invalid archive entry"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(internal_error_with("Invalid archive path"))?
            .to_path_buf();
        if unsafe_archive_path(&path) {
            return Err(UpdateError::bad_request("Unsafe archive path"));
        }

        if path.file_name().is_some_and(|name| name == APP_BINARY_NAME) {
            entry
                .unpack(&binary_path)
                .map_err(internal_error_with("Failed to extract binary"))?;
            found_binary = true;
            continue;
        }

        if let Some(relative) = web_dist_relative_path(&path) {
            let target = web_dist_dir.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(internal_error_with("Failed to create web asset dir"))?;
            }
            entry
                .unpack(&target)
                .map_err(internal_error_with("Failed to extract web asset"))?;
            found_web = true;
        }
    }

    if !found_binary {
        return Err(UpdateError::bad_request(
            "Release archive does not contain codex-proxy-rs",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&binary_path, fs::Permissions::from_mode(0o755))
            .map_err(internal_error_with("Failed to chmod binary"))?;
    }

    Ok(ExtractedRelease {
        binary_path,
        web_dist_dir: found_web.then_some(web_dist_dir),
    })
}

pub(super) fn replace_release_files(
    exe_path: &Path,
    web_dist_dir: &Path,
    extracted: ExtractedRelease,
) -> Result<(), UpdateError> {
    let web_backup = backup_path_for(web_dist_dir);
    let web_replaced = if let Some(new_web) = extracted.web_dist_dir {
        replace_dir(web_dist_dir, &web_backup, &new_web)?;
        true
    } else {
        false
    };

    let binary_backup = backup_path_for(exe_path);
    if binary_backup.exists()
        && let Err(error) = fs::remove_file(&binary_backup)
    {
        let mut rollback_errors = Vec::new();
        if web_replaced {
            collect_rollback_error(
                &mut rollback_errors,
                "restore web assets",
                restore_dir(web_dist_dir, &web_backup),
            );
        }
        return Err(update_error_with_rollback(
            "Failed to remove old binary backup",
            error,
            rollback_errors,
        ));
    }
    if let Err(error) = move_file(exe_path, &binary_backup) {
        let mut rollback_errors = Vec::new();
        if web_replaced {
            collect_rollback_error(
                &mut rollback_errors,
                "restore web assets",
                restore_dir(web_dist_dir, &web_backup),
            );
        }
        return Err(update_error_with_rollback(
            "Binary backup failed",
            error,
            rollback_errors,
        ));
    }
    if let Err(error) = move_file(&extracted.binary_path, exe_path) {
        let mut rollback_errors = Vec::new();
        if exe_path.exists() {
            collect_rollback_error(
                &mut rollback_errors,
                "remove partial replacement binary",
                fs::remove_file(exe_path),
            );
        }
        collect_rollback_error(
            &mut rollback_errors,
            "restore previous binary",
            move_file(&binary_backup, exe_path),
        );
        if web_replaced {
            collect_rollback_error(
                &mut rollback_errors,
                "restore web assets",
                restore_dir(web_dist_dir, &web_backup),
            );
        }
        return Err(update_error_with_rollback(
            "Binary replace failed",
            error,
            rollback_errors,
        ));
    }
    Ok(())
}

fn replace_dir(current: &Path, backup: &Path, replacement: &Path) -> Result<(), UpdateError> {
    if backup.exists() {
        fs::remove_dir_all(backup)
            .map_err(internal_error_with("Failed to remove old web backup"))?;
    }
    if current.exists() {
        move_dir(current, backup).map_err(internal_error_with("Failed to backup web assets"))?;
    }
    if let Err(error) = move_dir(replacement, current) {
        let mut rollback_errors = Vec::new();
        if backup.exists() {
            collect_rollback_error(
                &mut rollback_errors,
                "restore previous web assets",
                restore_dir(current, backup),
            );
        }
        return Err(update_error_with_rollback(
            "Failed to replace web assets",
            error,
            rollback_errors,
        ));
    }
    Ok(())
}

fn collect_rollback_error(errors: &mut Vec<String>, action: &'static str, result: io::Result<()>) {
    if let Err(error) = result {
        errors.push(format!("{action}: {error}"));
    }
}

fn update_error_with_rollback(
    context: &'static str,
    error: impl std::fmt::Display,
    rollback_errors: Vec<String>,
) -> UpdateError {
    if rollback_errors.is_empty() {
        return internal_error(context, error);
    }
    UpdateError::internal(format!(
        "{context}: {error}; rollback failed: {}",
        rollback_errors.join("; ")
    ))
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
        Err(error) if is_cross_device_link(&error) => {
            fs::copy(from, to)?;
            fs::remove_file(from)
        }
        Err(error) => Err(error),
    }
}

fn move_dir(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if is_cross_device_link(&error) => {
            copy_dir_all(from, to)?;
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

fn is_cross_device_link(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::CrossesDevices
}

fn backup_path_for(path: &Path) -> PathBuf {
    let mut backup = path.as_os_str().to_os_string();
    backup.push(".backup");
    PathBuf::from(backup)
}

fn web_dist_relative_path(path: &Path) -> Option<PathBuf> {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    for index in 0..components.len().saturating_sub(1) {
        if components[index] == "web"
            && components
                .get(index + 1)
                .is_some_and(|value| value == "dist")
        {
            return Some(components[index + 2..].iter().collect());
        }
        if components[index] == "dist" {
            return Some(components[index + 1..].iter().collect());
        }
    }
    None
}

fn unsafe_archive_path(path: &Path) -> bool {
    path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
}
