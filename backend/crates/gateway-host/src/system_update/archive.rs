//! Release tar.gz 安全解包与制品归一化

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use super::{OperationError, internal, invalid};

const APP_BINARY_NAME: &str = if cfg!(windows) {
    "codex-proxy-rs.exe"
} else {
    "codex-proxy-rs"
};
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
        .map_err(|error| internal("无法打开更新包").with_source(error))?;
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
        .map_err(|error| internal("无法读取更新包").with_source(error))?
    {
        let mut entry =
            entry.map_err(|error| internal("更新包内容损坏，请重新下载").with_source(error))?;
        let path = entry
            .path()
            .map_err(|error| internal("更新包中的文件路径无效").with_source(error))?
            .into_owned();
        if unsafe_archive_path(&path) {
            return Err(invalid("更新包包含不安全的文件路径，已停止更新"));
        }
        let official_relative = official_plugin_relative_path(&path)?;
        if !entry.header().entry_type().is_file() {
            if official_relative.is_some() {
                return Err(invalid("更新包中的官方插件资源不是普通文件"));
            }
            continue;
        }
        file_count = file_count.saturating_add(1);
        extracted_size = extracted_size.saturating_add(entry.header().size().unwrap_or(u64::MAX));
        if file_count > MAX_ARCHIVE_FILES || extracted_size > MAX_EXTRACTED_SIZE {
            return Err(invalid("更新包解压后的大小或文件数量超出限制"));
        }

        if path.file_name().is_some_and(|name| name == APP_BINARY_NAME) {
            if found_binary {
                return Err(invalid("更新包包含重复的程序文件"));
            }
            entry
                .unpack(&binary_path)
                .map_err(|error| internal("无法解压程序文件").with_source(error))?;
            found_binary = true;
            continue;
        }
        if let Some(relative) = official_relative {
            if !official_files.insert(relative.clone()) {
                return Err(invalid("更新包包含重复的官方插件文件"));
            }
            fs::create_dir_all(&official_plugins_dir)
                .map_err(|error| internal("无法创建官方插件目录").with_source(error))?;
            let target = official_plugins_dir.join(&relative);
            entry
                .unpack(&target)
                .map_err(|error| internal("无法解压官方插件文件").with_source(error))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&target, fs::Permissions::from_mode(0o444))
                    .map_err(|error| internal("无法设置官方插件文件权限").with_source(error))?;
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
                fs::create_dir_all(parent)
                    .map_err(|error| internal("无法创建管理页面目录").with_source(error))?;
            }
            entry
                .unpack(&target)
                .map_err(|error| internal("无法解压管理页面文件").with_source(error))?;
            found_web = true;
        }
    }
    if !found_binary {
        return Err(invalid("更新包缺少网关程序文件"));
    }
    if !found_official_manifest {
        return Err(invalid("更新包缺少官方插件发行清单"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&binary_path, fs::Permissions::from_mode(0o755))
            .map_err(|error| internal("无法设置程序执行权限").with_source(error))?;
    }
    Ok(ExtractedRelease {
        binary_path,
        web_dist_dir: found_web.then_some(web_dist_dir),
        official_plugins_dir,
    })
}

fn unsafe_archive_path(path: &Path) -> bool {
    path.is_absolute()
        || path.components().any(|component| match component {
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => true,
            #[cfg(windows)]
            Component::Normal(name) => unsafe_windows_name(name),
            _ => false,
        })
}

#[cfg(windows)]
fn unsafe_windows_name(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    // 防止归档路径被 Win32 解释为设备、NTFS 备用数据流或另一个规范化文件名
    if name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|value| value < ' ' || "<>:\"|?*".contains(value))
    {
        return true;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
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
            return Err(invalid("更新包中的官方插件目录结构不符合要求"));
        }
        let file_name = relative[0];
        let is_manifest = file_name == "plugin-release-manifest.json";
        let is_archive = file_name
            .to_str()
            .is_some_and(|value| value.ends_with(".tar.gz"));
        if !is_manifest && !is_archive {
            return Err(invalid("更新包包含不支持的官方插件文件类型"));
        }
        return Ok(Some(PathBuf::from(file_name)));
    }
    Ok(None)
}
