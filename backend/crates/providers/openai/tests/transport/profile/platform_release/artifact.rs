use super::xz::compress as xz;
use provider_openai::transport::profile::platform_release::{
    artifact::{read_linux, read_windows},
    xz::IndexedXz,
};
use serde_json::json;
use std::io::{Cursor, Read, Write};

fn core(windows: bool, arm: bool) -> Vec<u8> {
    let mut bytes = vec![0; 8192];
    if windows {
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&128_u32.to_le_bytes());
        bytes[128..132].copy_from_slice(b"PE\0\0");
        bytes[132..134].copy_from_slice(&(if arm { 0xaa64_u16 } else { 0x8664 }).to_le_bytes());
    } else {
        bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
        bytes[18..20].copy_from_slice(&(if arm { 183_u16 } else { 62 }).to_le_bytes());
    }
    let marker = b"codex-mcp-client/0.155.0-alpha.9response";
    bytes[4088..4088 + marker.len()].copy_from_slice(marker);
    bytes
}
fn asar() -> Vec<u8> {
    let package = serde_json::to_vec(&json!({"name":"openai-codex-electron", "version":"26.915.31029", "codexBuildNumber":"9771", "codexBuildFlavor":"prod"})).unwrap();
    let header = serde_json::to_vec(
        &json!({"files":{"package.json":{"offset":"13", "size":package.len()}}}),
    )
    .unwrap();
    let padded = (header.len() + 3) & !3;
    let mut bytes = Vec::new();
    for value in [
        4,
        (padded + 8) as u32,
        (padded + 4) as u32,
        header.len() as u32,
    ] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend(header);
    bytes.resize(16 + padded + 13, 0);
    bytes.extend(package);
    bytes
}
fn zip(arm: bool, payload: &[u8]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    let manifest = format!(
        "<Package><Identity Name=\"OpenAI.Codex\" ProcessorArchitecture=\"{}\" Version=\"26.908.9136.0\"/></Package>",
        if arm { "arm64" } else { "x64" }
    );
    for (name, data) in [
        ("AppxManifest.xml", manifest.as_bytes()),
        ("app/resources/app.asar", &asar()),
        ("app/resources/codex.exe", payload),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
fn deb(arm: bool) -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    for (path, data) in [
        ("./usr/lib/chatgpt/resources/app.asar", asar()),
        ("./usr/lib/chatgpt/resources/codex", core(false, arm)),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, path, data.as_slice()).unwrap();
    }
    let data = xz(&tar.into_inner().unwrap());
    let mut result = b"!<arch>\n".to_vec();
    for (name, data) in [
        ("debian-binary", b"2.0\n".as_slice()),
        ("data.tar.xz", data.as_slice()),
    ] {
        result.extend_from_slice(
            format!(
                "{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                0,
                0,
                0,
                "100644",
                data.len()
            )
            .as_bytes(),
        );
        result.extend_from_slice(data);
        if data.len() % 2 != 0 {
            result.push(b'\n');
        }
    }
    result
}

#[test]
fn windows_and_linux_read_versions_from_the_same_package_and_check_architecture() {
    for (arch, arm) in [("x86_64", false), ("arm64", true)] {
        let windows = read_windows(Cursor::new(zip(arm, &core(true, arm))), arch).unwrap();
        let linux = read_linux(Cursor::new(deb(arm)), arch).unwrap();
        assert_eq!(windows, linux);
        assert_eq!(windows.codex_version, "0.155.0-alpha.9");
        assert_eq!(windows.desktop_version.as_deref(), Some("26.915.31029"));
        assert_eq!(windows.desktop_build.as_deref(), Some("9771"));
        let other = if arm { "x86_64" } else { "arm64" };
        assert!(read_windows(Cursor::new(zip(arm, &core(true, arm))), other).is_err());
        assert!(read_windows(Cursor::new(zip(arm, &core(true, !arm))), arch).is_err());
        assert!(read_linux(Cursor::new(deb(arm)), other).is_err());
    }
}
#[test]
fn truncated_or_corrupt_archives_do_not_publish_versions() {
    let zip = zip(false, &core(true, false));
    let deb = deb(false);
    for bytes in [zip[..zip.len() - 20].to_vec(), vec![0; 100]] {
        assert!(read_windows(Cursor::new(bytes), "x86_64").is_err());
    }
    for bytes in [deb[..deb.len() - 20].to_vec(), vec![0; 100]] {
        assert!(read_linux(Cursor::new(bytes), "x86_64").is_err());
    }
    let bytes = xz(&vec![1; 50_000]);
    for offset in [8, bytes.len() - 1, bytes.len() - 16, 30] {
        let mut broken = bytes.clone();
        broken[offset] ^= 1;
        let result = IndexedXz::new(Cursor::new(&broken), 0, broken.len() as u64)
            .and_then(|mut reader| reader.read_to_end(&mut Vec::new()));
        assert!(result.is_err(), "corruption at {offset}");
    }
}
