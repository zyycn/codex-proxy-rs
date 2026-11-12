//! 验证 Desktop 制品内嵌 Core 版本扫描与 ZIP 目录约束

use provider_openai::transport::profile::desktop_artifact::{
    CodexDesktopArtifactError, CoreVersionScanner, find_core_entry, parse_content_range,
};

#[tokio::test]
async fn bundled_core_probe_uses_the_injected_proxy_client() {
    use provider_openai::transport::profile::{
        CodexDesktopRelease, CodexDesktopReleaseTransport, OfficialCodexDesktopReleaseTransport,
    };
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!("http://{}", listener.local_addr().unwrap())).unwrap())
        .build()
        .unwrap();
    let transport = OfficialCodexDesktopReleaseTransport::new(client).unwrap();
    let release = CodexDesktopRelease {
        version: "26.803.81509".into(),
        build: "6415".into(),
        published_at: None,
        minimum_system_version: None,
        hardware_requirements: None,
        download_url: Some("https://persistent.oaistatic.com/codex-app-prod/fixture.zip".into()),
        download_size: Some(100),
        signature_present: true,
    };
    let proxy = async {
        let (socket, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(socket);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(line, "CONNECT persistent.oaistatic.com:443 HTTP/1.1\r\n");
        reader
            .get_mut()
            .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(transport.fetch_bundled_core_version(&release), proxy)
    })
    .await
    .unwrap();
    assert!(result.is_err());
}

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
fn central_directory_with_only_cli_launcher_is_missing_core() {
    let central = deflated_entry(b"ChatGPT.app/Contents/Resources/codex-cli/bin/codex");
    assert!(matches!(
        find_core_entry(&central, 1),
        Err(CodexDesktopArtifactError::MissingCore)
    ));
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
