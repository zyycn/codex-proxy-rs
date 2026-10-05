//! Release tar.gz 安全解包与制品归一化

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use super::{OperationError, internal, invalid};

const APP_BINARY_NAME: &str = "codex-proxy-rs";
const MAX_EXTRACTED_SIZE: u64 = 1024 * 1024 * 1024;
const MAX_ARCHIVE_FILES: usize = 20_000;

#[derive(Debug)]
pub(crate) struct ExtractedRelease {
    pub(crate) binary_path: PathBuf,
    pub(crate) web_dist_dir: Option<PathBuf>,
    pub(crate) official_plugins_dir: PathBuf,
}

pub(crate) fn extract_release(
    archive_path: &Path,
    temp_dir: &Path,
) -> Result<ExtractedRelease, OperationError> {
    let file = fs::File::open(archive_path)
        .map_err(|error| internal(format!("failed to open release archive: {error}")))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let binary_path = temp_dir.join(APP_BINARY_NAME);
    let web_dist_dir = temp_dir.join("web-dist");
    let official_plugins_dir = temp_dir.join("official-plugins");
    let mut found_binary = false;
    let mut found_web = false;
    let mut found_official_manifest = false;
    let mut official_files = BTreeSet::new();
    let mut extracted_size = 0_u64;
    let mut file_count = 0_usize;

    for entry in archive
        .entries()
        .map_err(|error| internal(format!("failed to read release archive: {error}")))?
    {
        let mut entry =
            entry.map_err(|error| internal(format!("invalid archive entry: {error}")))?;
        let path = entry
            .path()
            .map_err(|error| internal(format!("invalid archive path: {error}")))?
            .into_owned();
        if unsafe_archive_path(&path) {
            return Err(invalid("release archive contains an unsafe path"));
        }
        let official_relative = official_plugin_relative_path(&path)?;
        if !entry.header().entry_type().is_file() {
            if official_relative.is_some() {
                return Err(invalid(
                    "release archive contains a non-file official plugin asset",
                ));
            }
            continue;
        }
        file_count = file_count.saturating_add(1);
        extracted_size = extracted_size.saturating_add(entry.header().size().unwrap_or(u64::MAX));
        if file_count > MAX_ARCHIVE_FILES || extracted_size > MAX_EXTRACTED_SIZE {
            return Err(invalid("release archive expands beyond safety limits"));
        }

        if path.file_name().is_some_and(|name| name == APP_BINARY_NAME) {
            if found_binary {
                return Err(invalid("release archive contains duplicate binaries"));
            }
            entry
                .unpack(&binary_path)
                .map_err(|error| internal(format!("failed to extract binary: {error}")))?;
            found_binary = true;
            continue;
        }
        if let Some(relative) = official_relative {
            if !official_files.insert(relative.clone()) {
                return Err(invalid(
                    "release archive contains a duplicate official plugin asset",
                ));
            }
            fs::create_dir_all(&official_plugins_dir).map_err(|error| {
                internal(format!("failed to create official plugin dir: {error}"))
            })?;
            let target = official_plugins_dir.join(&relative);
            entry.unpack(&target).map_err(|error| {
                internal(format!("failed to extract official plugin asset: {error}"))
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&target, fs::Permissions::from_mode(0o444)).map_err(
                    |error| internal(format!("failed to protect official plugin asset: {error}")),
                )?;
            }
            found_official_manifest |= relative == Path::new("plugin-release-manifest.json");
            continue;
        }
        if let Some(relative) = web_dist_relative_path(&path) {
            if relative.as_os_str().is_empty() {
                continue;
            }
            let target = web_dist_dir.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    internal(format!("failed to create web asset dir: {error}"))
                })?;
            }
            entry
                .unpack(&target)
                .map_err(|error| internal(format!("failed to extract web asset: {error}")))?;
            found_web = true;
        }
    }
    if !found_binary {
        return Err(invalid("release archive does not contain codex-proxy-rs"));
    }
    if !found_official_manifest {
        return Err(invalid(
            "release archive does not contain the official plugin manifest",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&binary_path, fs::Permissions::from_mode(0o755))
            .map_err(|error| internal(format!("failed to chmod binary: {error}")))?;
    }
    Ok(ExtractedRelease {
        binary_path,
        web_dist_dir: found_web.then_some(web_dist_dir),
        official_plugins_dir,
    })
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

fn web_dist_relative_path(path: &Path) -> Option<PathBuf> {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_os_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    for index in 0..components.len() {
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

fn official_plugin_relative_path(path: &Path) -> Result<Option<PathBuf>, OperationError> {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    for index in 0..components.len() {
        if components[index] != "plugins"
            || components
                .get(index + 1)
                .is_none_or(|value| *value != "official")
        {
            continue;
        }
        let relative = &components[index + 2..];
        if relative.is_empty() {
            return Ok(None);
        }
        if relative.len() != 1 {
            return Err(invalid(
                "release archive contains a nested official plugin asset",
            ));
        }
        let file_name = relative[0];
        let is_manifest = file_name == "plugin-release-manifest.json";
        let is_archive = file_name
            .to_str()
            .is_some_and(|value| value.ends_with(".tar.gz"));
        if !is_manifest && !is_archive {
            return Err(invalid(
                "release archive contains an unexpected official plugin asset",
            ));
        }
        return Ok(Some(PathBuf::from(file_name)));
    }
    Ok(None)
}
