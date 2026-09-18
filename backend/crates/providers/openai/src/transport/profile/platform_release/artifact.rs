//! 从 Windows MSIX 和 Linux DEB 的同一制品读取应用元数据与 bundled Core。

use super::super::desktop_artifact::CoreVersionScanner;
use super::super::selection::{ClientPlatform, ClientRelease};
use super::range::invalid;
use super::xz::IndexedXz;
use serde::Deserialize;
use std::io::{self, Read, Seek, SeekFrom};

const MAX_CORE: u64 = 512 * 1024 * 1024;
const MAX_ASAR_PREFIX: u64 = 128 * 1024 * 1024;

pub fn read_windows<R: Read + Seek>(source: R, arch: &str) -> io::Result<ClientRelease> {
    let mut zip = zip::ZipArchive::new(source).map_err(|_| invalid())?;
    if zip.len() > 20_000 {
        return Err(invalid());
    }
    let manifest = {
        let file = zip.by_name("AppxManifest.xml").map_err(|_| invalid())?;
        let mut text = String::new();
        file.take(128 * 1024 + 1).read_to_string(&mut text)?;
        if text.len() > 128 * 1024 {
            return Err(invalid());
        }
        text
    };
    let document = roxmltree::Document::parse(manifest.trim_start_matches('\u{feff}'))
        .map_err(|_| invalid())?;
    let identity = document
        .descendants()
        .find(|n| n.has_tag_name("Identity"))
        .ok_or_else(invalid)?;
    let expected_arch = match arch {
        "arm64" => "arm64",
        "x86_64" => "x64",
        _ => return Err(invalid()),
    };
    if identity.attribute("Name") != Some("OpenAI.Codex")
        || identity.attribute("ProcessorArchitecture") != Some(expected_arch)
    {
        return Err(invalid());
    }
    let mut release = {
        let mut file = zip
            .by_name("app/resources/app.asar")
            .map_err(|_| invalid())?;
        let size = file.size();
        let (offset, length, consumed) = asar_package_location(&mut file, size)?;
        if offset > MAX_ASAR_PREFIX {
            return Err(invalid());
        }
        if io::copy(&mut file.by_ref().take(offset - consumed), &mut io::sink())?
            != offset - consumed
        {
            return Err(invalid());
        }
        read_package(file, length)?
    };
    let file = zip
        .by_name("app/resources/codex.exe")
        .map_err(|_| invalid())?;
    let size = file.size();
    release.codex_version = read_core(file, size, ClientPlatform::Windows, arch)?;
    Ok(release)
}

pub fn read_linux<R: Read + Seek>(mut source: R, arch: &str) -> io::Result<ClientRelease> {
    let total = source.seek(SeekFrom::End(0))?;
    source.seek(SeekFrom::Start(0))?;
    let mut magic = [0; 8];
    source.read_exact(&mut magic)?;
    if &magic != b"!<arch>\n" {
        return Err(invalid());
    }
    let mut offset = 8;
    let mut data = None;
    for _ in 0..16 {
        if offset + 60 > total {
            break;
        }
        source.seek(SeekFrom::Start(offset))?;
        let mut header = [0; 60];
        source.read_exact(&mut header)?;
        if &header[58..] != b"`\n" {
            return Err(invalid());
        }
        let size = std::str::from_utf8(&header[48..58])
            .map_err(|_| invalid())?
            .trim()
            .parse::<u64>()
            .map_err(|_| invalid())?;
        let end = offset
            .checked_add(60)
            .and_then(|v| v.checked_add(size))
            .filter(|end| *end <= total)
            .ok_or_else(invalid)?;
        if std::str::from_utf8(&header[..16])
            .map_err(|_| invalid())?
            .trim()
            == "data.tar.xz"
        {
            data = Some((offset + 60, size));
            break;
        }
        offset = end + size % 2;
    }
    let (offset, size) = data.ok_or_else(invalid)?;
    let xz = IndexedXz::new(source, offset, size)?;
    let mut tar = tar::Archive::new(xz);
    let mut asar = None;
    let mut core = None;
    for entry in tar.entries_with_seek()?.take(30_000) {
        let entry = entry?;
        let path = entry.path()?;
        let path = path.to_str().ok_or_else(invalid)?.trim_start_matches("./");
        let target = match path {
            "usr/lib/chatgpt/resources/app.asar" => Some(&mut asar),
            "usr/lib/chatgpt/resources/codex" => Some(&mut core),
            _ => None,
        };
        if let Some(target) = target {
            if !entry.header().entry_type().is_file() || target.is_some() {
                return Err(invalid());
            }
            *target = Some((entry.raw_file_position(), entry.size()));
        }
        if asar.is_some() && core.is_some() {
            break;
        }
    }
    let mut xz = tar.into_inner();
    let (asar_offset, asar_size) = asar.ok_or_else(invalid)?;
    xz.seek(SeekFrom::Start(asar_offset))?;
    let (offset, length, _) = asar_package_location(&mut xz, asar_size)?;
    xz.seek(SeekFrom::Start(asar_offset + offset))?;
    let mut release = read_package(&mut xz, length)?;
    let (offset, size) = core.ok_or_else(invalid)?;
    xz.seek(SeekFrom::Start(offset))?;
    release.codex_version = read_core(xz, size, ClientPlatform::Linux, arch)?;
    Ok(release)
}

fn asar_package_location(reader: &mut impl Read, size: u64) -> io::Result<(u64, u64, u64)> {
    let mut prefix = [0; 16];
    reader.read_exact(&mut prefix)?;
    let u32_at = |at| -> io::Result<u32> {
        Ok(u32::from_le_bytes(
            prefix[at..at + 4].try_into().map_err(|_| invalid())?,
        ))
    };
    let header_size = u64::from(u32_at(4)?);
    let json_size = u64::from(u32_at(12)?);
    if u32_at(0)? != 4
        || !(8..=16 * 1024 * 1024).contains(&header_size)
        || json_size > header_size - 8
        || header_size + 8 > size
    {
        return Err(invalid());
    }
    let mut json = vec![0; json_size as usize];
    reader.read_exact(&mut json)?;
    let header: serde_json::Value = serde_json::from_slice(&json).map_err(|_| invalid())?;
    let package = &header["files"]["package.json"];
    if package.get("unpacked").is_some() || package.get("link").is_some() {
        return Err(invalid());
    }
    let offset = package["offset"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .and_then(|offset| offset.checked_add(header_size + 8))
        .ok_or_else(invalid)?;
    let length = package["size"]
        .as_u64()
        .filter(|size| *size > 0 && *size <= 256 * 1024)
        .ok_or_else(invalid)?;
    if offset.checked_add(length).is_none_or(|end| end > size) {
        return Err(invalid());
    }
    Ok((offset, length, 16 + json_size))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Package {
    name: String,
    version: String,
    codex_build_number: String,
    codex_build_flavor: String,
}

fn read_package(reader: impl Read, length: u64) -> io::Result<ClientRelease> {
    let mut bytes = Vec::new();
    reader.take(length).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != length {
        return Err(invalid());
    }
    let package: Package = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if package.name != "openai-codex-electron"
        || package.codex_build_flavor != "prod"
        || semver::Version::parse(&package.version).is_err()
        || package
            .codex_build_number
            .parse::<u64>()
            .ok()
            .is_none_or(|v| v == 0)
    {
        return Err(invalid());
    }
    Ok(ClientRelease {
        codex_version: String::new(),
        desktop_version: Some(package.version),
        desktop_build: Some(package.codex_build_number),
        verified_at: None,
    })
}

pub fn read_core(
    mut reader: impl Read,
    size: u64,
    platform: ClientPlatform,
    arch: &str,
) -> io::Result<String> {
    if !(4096..=MAX_CORE).contains(&size) {
        return Err(invalid());
    }
    let mut header = [0; 4096];
    reader.read_exact(&mut header)?;
    let matches = match (platform, arch) {
        (ClientPlatform::Linux, "x86_64" | "arm64") => {
            let machine = if arch == "arm64" { 183_u16 } else { 62 };
            &header[..6] == b"\x7fELF\x02\x01" && header[18..20] == machine.to_le_bytes()
        }
        (ClientPlatform::Windows, "x86_64" | "arm64") => {
            let offset =
                u32::from_le_bytes(header[60..64].try_into().map_err(|_| invalid())?) as usize;
            let machine = if arch == "arm64" { 0xaa64_u16 } else { 0x8664 };
            &header[..2] == b"MZ"
                && header.get(offset..offset.saturating_add(4)) == Some(b"PE\0\0")
                && header.get(offset.saturating_add(4)..offset.saturating_add(6))
                    == Some(machine.to_le_bytes().as_slice())
        }
        _ => false,
    };
    if !matches {
        return Err(invalid());
    }
    let mut scanner = CoreVersionScanner::default();
    if let Some(version) = scanner.push(&header).map_err(|_| invalid())? {
        return Ok(version);
    }
    let mut remaining = reader.take(size - 4096);
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = remaining.read(&mut buffer)?;
        if count == 0 {
            return Err(invalid());
        }
        if let Some(version) = scanner.push(&buffer[..count]).map_err(|_| invalid())? {
            return Ok(version);
        }
    }
}
