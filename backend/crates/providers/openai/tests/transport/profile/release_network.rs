//! 验证官方版本与制品检测实际经过选定出口，并支持运行中切换

use super::*;
use gateway_core::account::OutboundProxy;
use provider_openai::transport::profile::cli_release::CliReleaseService;
use provider_openai::transport::profile::platform_release::{
    DesktopArtifactTransport, OfficialDesktopArtifactTransport, TARGETS,
};
use provider_openai::transport::profile::{OfficialCodexDesktopReleaseTransport, ReleaseNetwork};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::TcpListener;

#[tokio::test]
async fn official_release_requests_follow_selected_proxy_after_switch() {
    let network = ReleaseNetwork::new().unwrap();
    let desktop = OfficialCodexDesktopReleaseTransport::new(network.clone()).unwrap();
    let cli = CliReleaseService::new(
        ProviderKind::new("openai").unwrap(),
        CodexWireProfileState::new(wire_profile()),
        Arc::new(ArtifactProfiles::default()),
        network.clone(),
    );
    let platform = OfficialDesktopArtifactTransport::new(network.clone());
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
    for _ in 0..2 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy =
            OutboundProxy::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        network.configure(Some(&proxy)).unwrap();
        // 拒绝 CONNECT 即可验证出口，不下载真实官方制品
        let proxy = async {
            for host in [
                "persistent.oaistatic.com",
                "persistent.oaistatic.com",
                "registry.npmjs.org",
                "persistent.oaistatic.com",
                "persistent.oaistatic.com",
            ] {
                let (socket, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(socket);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                assert_eq!(line, format!("CONNECT {host}:443 HTTP/1.1\r\n"));
                reader
                    .get_mut()
                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                    .await
                    .unwrap();
            }
        };
        let requests = async {
            assert!(desktop.fetch().await.is_err());
            assert!(desktop.fetch_bundled_core_version(&release).await.is_err());
            assert!(cli.refresh().await.is_err());
            for target in [TARGETS[0], TARGETS[2]] {
                assert!(platform.fetch(target, None).await.is_err());
            }
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(requests, proxy);
        })
        .await
        .unwrap();
    }
}
