//! 验证 Desktop 制品内嵌 Core 版本扫描与 ZIP 目录约束

use provider_openai::transport::profile::desktop_artifact::{
    CodexDesktopArtifactError, CoreVersionScanner, find_core_entry, parse_content_range,
};

#[test]
fn scanner_accepts_stable_and_multi_part_alpha_versions_across_chunks() {
    for expected in ["0.148.0", "0.147.0-alpha.6.6"] {
        let bytes = format!("prefixcodex-mcp-client/{expected}OAuth suffix");
        let split = bytes.find("client").expect("split point") + 3;
        let mut scanner = CoreVersionScanner::default();
        assert_eq!(
            scanner.push(&bytes.as_bytes()[..split]).expect("scan"),
            None
        );
        assert_eq!(
            scanner.push(&bytes.as_bytes()[split..]).expect("scan"),
            Some(expected.to_owned())
        );
    }
}

#[test]
fn scanner_rejects_unrecognized_prerelease_shapes() {
    let mut scanner = CoreVersionScanner::default();
    assert!(
        scanner
            .push(b"codex-mcp-client/0.149.0-nightly.1 next")
            .expect("scan")
            .is_none()
    );
}

#[test]
fn scanner_rejects_build_metadata_instead_of_truncating_the_version() {
    for version in ["0.149.0+desktop.1", "0.149.0-alpha.1+desktop.1"] {
        let mut scanner = CoreVersionScanner::default();
        let bytes = format!("codex-mcp-client/{version}OAuth");
        assert!(scanner.push(bytes.as_bytes()).expect("scan").is_none());
    }
}

#[test]
fn scanner_skips_an_invalid_marker_before_the_bundled_version() {
    let mut scanner = CoreVersionScanner::default();
    assert_eq!(
        scanner
            .push(b"codex-mcp-client/not-a-version codex-mcp-client/0.147.0-alpha.6.6OAuth",)
            .expect("scan"),
        Some("0.147.0-alpha.6.6".to_owned())
    );
}

#[test]
fn central_directory_requires_one_deflated_core_entry() {
    let name = b"ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex";
    let central = deflated_entry(name);
    let entry = find_core_entry(&central, 1).expect("core entry");
    assert_eq!(entry.name, name);
    assert_eq!(entry.compressed_size, 100);
    assert_eq!(entry.uncompressed_size, 200);
    assert_eq!(entry.local_header_offset, 50);
}

#[test]
fn central_directory_finds_app_bundled_core_among_cli_helpers() {
    let name = b"ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex";
    let central = [
        deflated_entry(b"ChatGPT.app/Contents/Resources/codex-cli/bin/codex"),
        deflated_entry(name),
        deflated_entry(b"ChatGPT.app/Contents/Resources/codex-cli/bin/codex-code-mode-host"),
    ]
    .concat();

    assert_eq!(
        find_core_entry(&central, 3).expect("app bundled core").name,
        name
    );
}

#[test]
fn central_directory_rejects_duplicate_core_entries() {
    let name = b"ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex";
    let central = [deflated_entry(name), deflated_entry(name)].concat();
    assert!(matches!(
        find_core_entry(&central, 2),
        Err(CodexDesktopArtifactError::MissingCore)
    ));
}

#[test]
fn central_directory_rejects_legacy_core_and_cli_launcher_paths() {
    for name in [
        b"ChatGPT.app/Contents/Resources/codex".as_slice(),
        b"ChatGPT.app/Contents/Resources/codex-cli/bin/codex",
    ] {
        let central = deflated_entry(name);
        assert!(matches!(
            find_core_entry(&central, 1),
            Err(CodexDesktopArtifactError::MissingCore)
        ));
    }
}

fn deflated_entry(name: &[u8]) -> Vec<u8> {
    let mut central = vec![0_u8; 46 + name.len()];
    central[..4].copy_from_slice(b"PK\x01\x02");
    central[10..12].copy_from_slice(&8_u16.to_le_bytes());
    central[20..24].copy_from_slice(&100_u32.to_le_bytes());
    central[24..28].copy_from_slice(&200_u32.to_le_bytes());
    central[28..30].copy_from_slice(
        &u16::try_from(name.len())
            .expect("name length")
            .to_le_bytes(),
    );
    central[42..46].copy_from_slice(&50_u32.to_le_bytes());
    central[46..].copy_from_slice(name);

    central
}

#[test]
fn content_range_parser_is_strict() {
    assert_eq!(parse_content_range("bytes 10-19/100"), Some((10, 19, 100)));
    assert_eq!(parse_content_range("bytes */100"), None);
    assert_eq!(parse_content_range("10-19/100"), None);
}
