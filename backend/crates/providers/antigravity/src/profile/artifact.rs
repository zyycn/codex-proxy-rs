//! 从官方发布包核对版本和目标平台，不执行下载的程序或发起模型请求

use super::archive::{CancellableFile, RemoteArtifact, RemoteZip, bounded, download, invalid};
use super::selection::{Architecture, ClientKind, Platform, ReleaseTarget};
use gateway_core::task::WorkerTaskError;
use std::io::Read;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Release {
    pub(crate) version: String,
    pub(crate) build: Option<String>,
    pub(crate) url: String,
    pub(crate) sha512: Option<String>,
}

const MAX_ASAR_BYTES: usize = 16 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;

pub(crate) async fn verify(
    client: &reqwest::Client,
    target: ReleaseTarget,
    version: &str,
    url: &str,
    sha512: Option<&str>,
) -> Result<(), WorkerTaskError> {
    match (target.client, target.platform) {
        (ClientKind::Desktop, Platform::Macos) => {
            let url = url.strip_suffix(".dmg").ok_or_else(invalid)?;
            let archive = RemoteZip::open(client, format!("{url}.zip")).await?;
            let asar = archive
                .read(
                    "Antigravity.app/Contents/Resources/app.asar",
                    MAX_ASAR_BYTES,
                )
                .await?;
            if asar_version(&asar)? != version {
                return Err(invalid());
            }
            let executable = archive
                .read("Antigravity.app/Contents/MacOS/Antigravity", 1024 * 1024)
                .await?;
            architecture(&executable, target)
        }
        (ClientKind::Desktop, Platform::Windows) => {
            let artifact = RemoteArtifact::open(client, url.to_owned()).await?;
            let header = artifact.prefix(1024 * 1024).await?;
            let (offset, size) = pe_resources(&header)?;
            let resources = artifact.read(offset, size).await?;
            windows_version(&resources, version)
        }
        _ => {
            let file = download(client, url, sha512).await?;
            // 取消 worker 时同步中止阻塞解包，临时文件由解析任务退出时关闭
            let (file, _guard) = CancellableFile::new(file);
            let version = version.to_owned();
            tokio::task::spawn_blocking(move || {
                if target.client == ClientKind::Cli && target.platform == Platform::Windows {
                    verify_binary(file, target, &version)
                } else {
                    verify_tar(file, target, &version)
                }
            })
            .await
            .map_err(|e| invalid().with_source(e))?
        }
    }
}

pub(crate) fn asar_version(body: &[u8]) -> Result<String, WorkerTaskError> {
    let header_length = u32_at(body, 4)? as usize;
    let json_length = u32_at(body, 12)? as usize;
    let data_start = header_length.checked_add(8).ok_or_else(invalid)?;
    if json_length > MAX_ASAR_BYTES || json_length.checked_add(16).is_none_or(|n| n > data_start) {
        return Err(invalid());
    }
    let header: serde_json::Value =
        serde_json::from_slice(body.get(16..16 + json_length).ok_or_else(invalid)?)
            .map_err(|e| invalid().with_source(e))?;
    let package = &header["files"]["package.json"];
    let offset: usize = package["offset"]
        .as_str()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let size = package["size"]
        .as_u64()
        .filter(|n| *n <= 64 * 1024)
        .ok_or_else(invalid)? as usize;
    let start = data_start.checked_add(offset).ok_or_else(invalid)?;
    let end = start.checked_add(size).ok_or_else(invalid)?;
    let package: serde_json::Value =
        serde_json::from_slice(body.get(start..end).ok_or_else(invalid)?)
            .map_err(|e| invalid().with_source(e))?;
    if package["name"] != "antigravity" {
        return Err(invalid());
    }
    package["version"]
        .as_str()
        .filter(|v| super::selection::valid_version(v))
        .map(str::to_owned)
        .ok_or_else(invalid)
}

pub(crate) fn architecture(body: &[u8], target: ReleaseTarget) -> Result<(), WorkerTaskError> {
    let arm = target.arch == Architecture::Arm64;
    let matches = match target.platform {
        Platform::Macos => {
            body.starts_with(&[0xcf, 0xfa, 0xed, 0xfe])
                && u32_at(body, 4)? == if arm { 0x0100_000c } else { 0x0100_0007 }
        }
        Platform::Linux => {
            body.starts_with(b"\x7fELF\x02\x01") && u16_at(body, 18)? == if arm { 183 } else { 62 }
        }
        Platform::Windows => {
            let pe = pe_offset(body)?;
            u16_at(body, pe + 4)? == if arm { 0xaa64 } else { 0x8664 }
        }
    };
    if matches { Ok(()) } else { Err(invalid()) }
}

pub(crate) fn verify_tar(
    file: impl Read,
    target: ReleaseTarget,
    version: &str,
) -> Result<(), WorkerTaskError> {
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder.take(2 * 1024 * 1024 * 1024));
    let mut found_version = false;
    let mut found_executable = false;
    for (index, entry) in archive
        .entries()
        .map_err(|e| invalid().with_source(e))?
        .enumerate()
    {
        if index >= 16_384 {
            return Err(invalid());
        }
        let mut entry = entry.map_err(|e| invalid().with_source(e))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|e| invalid().with_source(e))?
            .into_owned();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if target.client == ClientKind::Cli && matches!(name, "antigravity" | "antigravity.exe") {
            if found_executable || entry.size() > MAX_BINARY_BYTES {
                return Err(invalid());
            }
            verify_binary(&mut entry, target, version)?;
            found_version = true;
            found_executable = true;
        } else if target.client == ClientKind::Desktop {
            if path.ends_with("resources/app.asar") {
                if found_version || asar_version(&bounded(&mut entry, MAX_ASAR_BYTES)?)? != version
                {
                    return Err(invalid());
                }
                found_version = true;
            } else if name == "antigravity" {
                if found_executable {
                    return Err(invalid());
                }
                let mut header = [0; 64];
                entry
                    .read_exact(&mut header)
                    .map_err(|e| invalid().with_source(e))?;
                architecture(&header, target)?;
                found_executable = true;
            }
        }
    }
    if found_version && found_executable {
        Ok(())
    } else {
        Err(invalid())
    }
}

pub(crate) fn verify_binary(
    mut reader: impl Read,
    target: ReleaseTarget,
    version: &str,
) -> Result<(), WorkerTaskError> {
    let mut header = [0; 1024];
    reader
        .read_exact(&mut header)
        .map_err(|e| invalid().with_source(e))?;
    architecture(&header, target)?;
    // 官方 CLI 把版本文件嵌入 Go 制品，保留其换行与终止字节避免匹配依赖版本
    let marker = format!("\0{version}\n\0").into_bytes();
    let mut tail = header.to_vec();
    let mut chunk = [0; 64 * 1024];
    let mut count = 1024u64;
    let mut found = false;
    loop {
        if tail.windows(marker.len()).any(|v| v == marker) {
            found = true;
        }
        let keep = tail.len().saturating_sub(marker.len());
        tail.drain(..keep);
        let size = reader
            .read(&mut chunk)
            .map_err(|e| invalid().with_source(e))?;
        if size == 0 {
            break;
        }
        count += size as u64;
        if count > MAX_BINARY_BYTES {
            return Err(invalid());
        }
        tail.extend_from_slice(&chunk[..size]);
    }
    if found { Ok(()) } else { Err(invalid()) }
}

fn pe_offset(body: &[u8]) -> Result<usize, WorkerTaskError> {
    if !body.starts_with(b"MZ") {
        return Err(invalid());
    }
    let offset = u32_at(body, 0x3c)? as usize;
    if body.get(offset..offset + 4) != Some(b"PE\0\0") {
        return Err(invalid());
    }
    Ok(offset)
}

pub(crate) fn pe_resources(body: &[u8]) -> Result<(u64, u64), WorkerTaskError> {
    let pe = pe_offset(body)?;
    let count = u16_at(body, pe + 6)? as usize;
    let start = pe + 24 + u16_at(body, pe + 20)? as usize;
    for i in 0..count.min(96) {
        let section = start + i * 40;
        if body.get(section..section + 8) == Some(b".rsrc\0\0\0") {
            return Ok((
                u64::from(u32_at(body, section + 20)?),
                u64::from(u32_at(body, section + 16)?),
            ));
        }
    }
    Err(invalid())
}

pub(crate) fn windows_version(body: &[u8], version: &str) -> Result<(), WorkerTaskError> {
    let parts: Vec<u16> = version
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| invalid())?;
    if parts.len() != 3 {
        return Err(invalid());
    }
    let high = u32::from(parts[0]) << 16 | u32::from(parts[1]);
    let low = u32::from(parts[2]) << 16;
    let mut found = false;
    for (offset, bytes) in body.windows(8).enumerate() {
        // 官方 Windows 安装器的 dwStrucVersion 为零，也接受标准的 0x00010000
        if bytes.starts_with(b"\xbd\x04\xef\xfe")
            && matches!(u32_at(body, offset + 4)?, 0 | 0x0001_0000)
        {
            if u32_at(body, offset + 8)? != high
                || u32_at(body, offset + 12)? != low
                || u32_at(body, offset + 16)? != high
                || u32_at(body, offset + 20)? != low
            {
                return Err(invalid());
            }
            found = true;
        }
    }
    if found { Ok(()) } else { Err(invalid()) }
}

fn u16_at(body: &[u8], offset: usize) -> Result<u16, WorkerTaskError> {
    Ok(u16::from_le_bytes(
        body.get(offset..offset + 2)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    ))
}
fn u32_at(body: &[u8], offset: usize) -> Result<u32, WorkerTaskError> {
    Ok(u32::from_le_bytes(
        body.get(offset..offset + 4)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    ))
}
