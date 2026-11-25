//! 发布包元数据与可执行文件的版本、边界和平台校验

use crate::profile::{
    artifact,
    selection::{Architecture, ClientKind, Platform, ReleaseTarget},
};
use std::io::{Cursor, Write};

pub(super) fn asar(version: &str) -> Vec<u8> {
    let package =
        serde_json::to_vec(&serde_json::json!({"name":"antigravity","version":version})).unwrap();
    let json = serde_json::to_vec(
        &serde_json::json!({"files":{"package.json":{"offset":"0","size":package.len()}}}),
    )
    .unwrap();
    let mut bytes = vec![0; 16];
    bytes[4..8].copy_from_slice(&(json.len() as u32 + 8).to_le_bytes());
    bytes[12..16].copy_from_slice(&(json.len() as u32).to_le_bytes());
    bytes.extend(json);
    bytes.extend(package);
    bytes
}

pub(super) fn executable(target: ReleaseTarget, version: &str) -> Vec<u8> {
    let mut bytes = vec![0; 65_536];
    let arm = target.arch == Architecture::Arm64;
    match target.platform {
        Platform::Macos => {
            bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
            bytes[4..8].copy_from_slice(
                &(if arm { 0x0100_000cu32 } else { 0x0100_0007u32 }).to_le_bytes(),
            );
        }
        Platform::Linux => {
            bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
            bytes[18..20].copy_from_slice(&(if arm { 183u16 } else { 62u16 }).to_le_bytes());
        }
        Platform::Windows => {
            bytes[..2].copy_from_slice(b"MZ");
            bytes[60..64].copy_from_slice(&128u32.to_le_bytes());
            bytes[128..132].copy_from_slice(b"PE\0\0");
            bytes[132..134]
                .copy_from_slice(&(if arm { 0xaa64u16 } else { 0x8664u16 }).to_le_bytes());
        }
    }
    // 把版本放在扫描块边界上，覆盖跨块字符串识别
    bytes.extend_from_slice(format!("{version}\n\0").as_bytes());
    bytes
}

pub(super) fn desktop_zip(version: &str) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("Antigravity.app/Contents/Resources/app.asar", options)
        .unwrap();
    zip.write_all(&asar(version)).unwrap();
    zip.start_file("Antigravity.app/Contents/MacOS/Antigravity", options)
        .unwrap();
    zip.write_all(&executable(
        crate::profile::Selection::default().target(),
        version,
    ))
    .unwrap();
    zip.finish().unwrap().into_inner()
}

#[test]
fn asar_rejects_wrong_package_truncation_and_invalid_offsets() {
    let valid = asar("2.35.1");
    assert_eq!(artifact::asar_version(&valid).unwrap(), "2.35.1");
    for end in [0, 15, valid.len() - 1] {
        assert!(artifact::asar_version(&valid[..end]).is_err());
    }
    let mut invalid = valid.clone();
    invalid[4..8].copy_from_slice(&0u32.to_le_bytes());
    assert!(artifact::asar_version(&invalid).is_err());
    let invalid = String::from_utf8_lossy(&valid)
        .replace("antigravity", "otherproduct")
        .into_bytes();
    assert!(artifact::asar_version(&invalid).is_err());
}

#[test]
fn cli_binary_requires_target_architecture_and_exact_embedded_version() {
    for platform in [Platform::Macos, Platform::Linux, Platform::Windows] {
        for arch in [Architecture::Arm64, Architecture::Amd64] {
            let target = ReleaseTarget {
                client: ClientKind::Cli,
                platform,
                arch,
            };
            let binary = executable(target, "1.5.16");
            assert!(artifact::verify_binary(binary.as_slice(), target, "1.5.16").is_ok());
            assert!(artifact::verify_binary(binary.as_slice(), target, "1.5.15").is_err());
            let other = ReleaseTarget {
                arch: if arch == Architecture::Arm64 {
                    Architecture::Amd64
                } else {
                    Architecture::Arm64
                },
                ..target
            };
            assert!(artifact::verify_binary(binary.as_slice(), other, "1.5.16").is_err());
        }
    }
}

#[test]
fn windows_resource_requires_consistent_file_and_product_versions() {
    let mut resource = b"\xbd\x04\xef\xfe\0\0\x01\0".to_vec();
    for word in [2u32 << 16 | 35, 1 << 16, 2 << 16 | 35, 1 << 16] {
        resource.extend(word.to_le_bytes());
    }
    assert!(artifact::windows_version(&resource, "2.35.1").is_ok());
    resource[4..8].fill(0);
    assert!(artifact::windows_version(&resource, "2.35.1").is_ok());
    assert!(artifact::windows_version(&resource, "2.35.2").is_err());
    resource[16] ^= 1;
    assert!(artifact::windows_version(&resource, "2.35.1").is_err());
}

#[test]
fn tar_requires_both_package_and_executable_and_rejects_wrong_version() {
    let target = ReleaseTarget {
        client: ClientKind::Desktop,
        platform: Platform::Linux,
        arch: Architecture::Amd64,
    };
    let make = |include_executable: bool| {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut files = vec![("Antigravity/resources/app.asar", asar("2.35.1"))];
        if include_executable {
            files.push(("Antigravity/antigravity", executable(target, "2.35.1")));
        }
        for (path, body) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, body.as_slice())
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    };
    assert!(artifact::verify_tar(make(true).as_slice(), target, "2.35.1").is_ok());
    assert!(artifact::verify_tar(make(true).as_slice(), target, "2.35.2").is_err());
    assert!(artifact::verify_tar(make(false).as_slice(), target, "2.35.1").is_err());
}
