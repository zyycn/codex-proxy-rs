//! 验证 HTTP 与 WebSocket 的 TLS 握手特征

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;

use bytes::{Buf as _, Bytes};

use super::*;

const CASE_ENV: &str = "CODEX_PROXY_TEST_HTTP_CLIENT_HELLO";
const CASE_COMPLETED: &str = "http-tls-client-hello-case-completed";
const REFERENCE_ENV: &str = "CODEX_PROXY_TEST_REFERENCE_BINARY";

// 基线来自官方 Codex 823ea830c0 的客户端源码，各路径重复采样五次
// HTTP 另与本机 Core 0.160.0 的采样核对，源码客户端与本项目还通过真实 models 请求验证
// HTTP/native-tls 的扩展顺序固定；WebSocket/rustls 会随机化扩展顺序和临时密钥
#[derive(Debug, PartialEq, Eq)]
struct ClientHello {
    legacy_version: u16,
    supported_versions: Vec<u16>,
    point_formats: Vec<u8>,
    psk_modes: Vec<u8>,
    cipher_suites: Vec<u16>,
    extensions: Vec<u16>,
    groups: Vec<u16>,
    signature_algorithms: Vec<u16>,
    key_shares: Vec<(u16, usize)>,
    alpn: Vec<String>,
}

#[test]
fn http_client_hellos_should_match_official_tls_transports() {
    if let Ok(case) = std::env::var(CASE_ENV) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let client = provider_openai::transport::build_reqwest_client().unwrap();
            let mut hello = capture_http_hello(client).await;
            if case == "custom-ca" {
                hello.extensions.sort_unstable();
                assert_eq!(hello, official_http_rustls_hello());
            } else {
                #[cfg(target_os = "linux")]
                let expected = official_linux_http_hello();
                // 官方无自定义 CA 时使用 reqwest 默认的原生 TLS；系统后端的握手随操作系统而变
                #[cfg(not(target_os = "linux"))]
                let expected =
                    capture_http_hello(reqwest::Client::builder().no_proxy().build().unwrap())
                        .await;
                assert_eq!(hello, expected);
            }
        });
        println!("\n{CASE_COMPLETED}");
        return;
    }

    run_isolated_cases(
        "transport::tls::http_client_hellos_should_match_official_tls_transports",
        &["native", "custom-ca"],
    );
}

#[test]
#[ignore = "requires CODEX_PROXY_TEST_REFERENCE_BINARY pointing to the official Codex executable"]
fn client_hellos_should_match_the_official_binary_on_this_platform() {
    let reference = std::env::var_os(REFERENCE_ENV)
        .map(std::path::PathBuf::from)
        .expect("set CODEX_PROXY_TEST_REFERENCE_BINARY to the official Codex executable")
        .canonicalize()
        .expect("official Codex executable path");
    if let Ok(case) = std::env::var(CASE_ENV) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            for sample in 0..3 {
                let mut expected = capture_official_hello(&reference, case == "websocket").await;
                let mut actual = if case == "websocket" {
                    capture_websocket_hello().await
                } else {
                    capture_http_hello(provider_openai::transport::build_reqwest_client().unwrap())
                        .await
                };
                // rustls 随机排列扩展，其他字段及各自列表的顺序仍逐项核对
                if case != "native" {
                    expected.extensions.sort_unstable();
                    actual.extensions.sort_unstable();
                }
                assert_eq!(actual, expected, "{case} ClientHello sample {sample}");
            }
        });
        println!("\n{CASE_COMPLETED}");
        return;
    }

    let version = Command::new(&reference)
        .arg("--version")
        .output()
        .expect("read official Codex version");
    assert!(version.status.success());
    println!(
        "TLS reference: {} on {}-{}",
        String::from_utf8_lossy(&version.stdout).trim(),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    run_isolated_cases(
        "transport::tls::client_hellos_should_match_the_official_binary_on_this_platform",
        &["native", "custom-ca", "websocket"],
    );
}

fn run_isolated_cases(test_name: &str, cases: &[&str]) {
    // Cargo 运行测试时可能注入 SSL_CERT_FILE，生产代码会把它视作自定义 CA 并切到 rustls；
    // 因此在清理相关环境变量的子进程里验证默认路径
    let current_exe = std::env::current_exe().expect("current test binary path");
    let directory = tempfile::tempdir().unwrap();
    let ca_path = directory.path().join("ca.pem");
    std::fs::write(&ca_path, TEST_CA_PEM).unwrap();
    for case in cases {
        let mut command = Command::new(&current_exe);
        command
            .arg("--exact")
            .arg(test_name)
            .arg("--include-ignored")
            .arg("--nocapture")
            .env(CASE_ENV, case)
            .env_remove(provider_openai::transport::tls::CODEX_CA_CERT_ENV)
            .env_remove(provider_openai::transport::tls::SSL_CERT_FILE_ENV);
        if *case == "custom-ca" {
            command.env(provider_openai::transport::tls::CODEX_CA_CERT_ENV, &ca_path);
        }
        let output = command.output().expect("run isolated TLS ClientHello case");
        assert!(
            output.status.success(),
            "isolated {case} TLS ClientHello case failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout
                .lines()
                .filter(|line| *line == CASE_COMPLETED)
                .count(),
            1,
            "isolated {case} TLS ClientHello case did not complete exactly once\nstdout:\n{stdout}"
        );
    }
}

async fn capture_official_hello(reference: &Path, websocket: bool) -> ClientHello {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "https://localhost:{}/v1",
        listener.local_addr().unwrap().port()
    );
    let directory = tempfile::tempdir().unwrap();
    // 官方进程只访问本机采集端点，握手后即终止，认证使用无效测试值
    let mut command = tokio::process::Command::new(reference);
    command
        .args([
            "exec",
            "--ignore-user-config",
            "--ignore-rules",
            "--ephemeral",
            "--skip-git-repo-check",
            "--sandbox",
            "read-only",
            "--json",
            "--model",
            "tls-probe",
        ])
        .current_dir(directory.path())
        .env("CPR_TLS_PROBE_API_KEY", "test-local-handshake-only")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        command.env_remove(name);
    }
    for setting in [
        "model_provider=\"cpr_tls_probe\"".to_owned(),
        "model_providers.cpr_tls_probe.name=\"CPR TLS Probe\"".to_owned(),
        format!("model_providers.cpr_tls_probe.base_url=\"{endpoint}\""),
        "model_providers.cpr_tls_probe.env_key=\"CPR_TLS_PROBE_API_KEY\"".to_owned(),
        "model_providers.cpr_tls_probe.wire_api=\"responses\"".to_owned(),
        format!("model_providers.cpr_tls_probe.supports_websockets={websocket}"),
        "model_providers.cpr_tls_probe.request_max_retries=0".to_owned(),
        "model_providers.cpr_tls_probe.stream_max_retries=0".to_owned(),
        "project_doc_max_bytes=0".to_owned(),
    ] {
        command.args(["--config", &setting]);
    }
    let mut child = command
        .arg("TLS handshake capture")
        .spawn()
        .expect("start official Codex TLS reference");
    let hello = timeout(Duration::from_secs(30), read_client_hello(listener)).await;
    let _ = child.kill().await;
    child
        .wait()
        .await
        .expect("reap official Codex TLS reference");
    hello.expect("official Codex ClientHello within timeout")
}

async fn capture_http_hello(client: reqwest::Client) -> ClientHello {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "https://localhost:{}/",
        listener.local_addr().unwrap().port()
    );
    let (hello, response) = timeout(Duration::from_secs(10), async {
        tokio::join!(read_client_hello(listener), client.get(url).send())
    })
    .await
    .expect("HTTP ClientHello within timeout");
    assert!(
        response.is_err(),
        "capture endpoint rejects TLS after ClientHello"
    );
    hello
}

#[tokio::test]
async fn websocket_client_hello_should_match_official_rustls_transport() {
    let mut hello = capture_websocket_hello().await;
    hello.extensions.sort_unstable();
    assert_eq!(hello, official_websocket_hello());
}

async fn capture_websocket_hello() -> ClientHello {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("wss://localhost:{}/", listener.local_addr().unwrap().port());
    let connector =
        provider_openai::transport::tls::maybe_build_rustls_client_config_with_custom_ca()
            .unwrap()
            .map(tokio_tungstenite::Connector::Rustls);
    let (hello, response) = timeout(Duration::from_secs(10), async {
        tokio::join!(
            read_client_hello(listener),
            tokio_tungstenite::connect_async_tls_with_config(url, None, false, connector)
        )
    })
    .await
    .expect("WebSocket ClientHello within timeout");
    assert!(
        response.is_err(),
        "capture endpoint rejects TLS after ClientHello"
    );
    hello
}

#[cfg(target_os = "linux")]
fn official_linux_http_hello() -> ClientHello {
    ClientHello {
        legacy_version: 771,
        supported_versions: vec![772, 771],
        point_formats: vec![0],
        psk_modes: vec![1],
        cipher_suites: vec![
            4866, 4867, 4865, 49196, 49200, 159, 52393, 52392, 52394, 49195, 49199, 158, 49188,
            49192, 107, 49187, 49191, 103, 49162, 49172, 57, 49161, 49171, 51, 157, 156, 61, 60,
            53, 47,
        ],
        extensions: vec![65281, 0, 11, 10, 35, 22, 23, 13, 43, 45, 51],
        groups: vec![4588, 29, 23, 30, 24, 25, 256, 257],
        signature_algorithms: vec![
            2309, 2310, 2308, 1027, 1283, 1539, 2055, 2056, 2074, 2075, 2076, 2057, 2058, 2059,
            2052, 2053, 2054, 1025, 1281, 1537, 771, 769, 770, 1026, 1282, 1538,
        ],
        key_shares: vec![(4588, 1216), (29, 32)],
        alpn: Vec::new(),
    }
}

fn official_websocket_hello() -> ClientHello {
    ClientHello {
        legacy_version: 771,
        supported_versions: vec![772, 771],
        point_formats: vec![0],
        psk_modes: vec![1],
        cipher_suites: vec![
            4866, 4865, 4867, 49196, 49195, 52393, 49200, 49199, 52392, 255,
        ],
        extensions: vec![0, 5, 10, 11, 13, 23, 35, 43, 45, 51],
        groups: vec![4588, 29, 23, 24],
        signature_algorithms: vec![
            1283, 1027, 1539, 2055, 2054, 2053, 2052, 1537, 1281, 1025, 2308, 2309, 2310,
        ],
        key_shares: vec![(4588, 1216), (29, 32)],
        alpn: Vec::new(),
    }
}

fn official_http_rustls_hello() -> ClientHello {
    let mut hello = official_websocket_hello();
    hello.extensions.push(16);
    hello.extensions.sort_unstable();
    hello.alpn = vec!["h2".to_owned(), "http/1.1".to_owned()];
    hello
}

async fn read_client_hello(listener: TcpListener) -> ClientHello {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut header = [0; 5];
    stream.read_exact(&mut header).await.unwrap();
    assert_eq!(header[0], 22, "TLS handshake record");
    let length = usize::from(u16::from_be_bytes([header[3], header[4]]));
    assert!(length <= 16_384, "bounded ClientHello record");
    let mut record = vec![0; length];
    stream.read_exact(&mut record).await.unwrap();
    stream.write_all(&[21, 3, 3, 0, 2, 2, 40]).await.unwrap();

    let mut hello = Bytes::from(record);
    assert_eq!(hello.get_u8(), 1, "ClientHello message");
    hello.advance(3); // 消息长度
    let legacy_version = hello.get_u16();
    hello.advance(32); // 随机数
    let session_id_len = usize::from(hello.get_u8());
    hello.advance(session_id_len);
    let cipher_suites = u16_values(take_vector(&mut hello));
    let compression_len = usize::from(hello.get_u8());
    hello.advance(compression_len);
    let mut extensions = take_vector(&mut hello);
    let mut extension_order = Vec::new();
    let mut values = BTreeMap::new();
    while extensions.has_remaining() {
        let kind = extensions.get_u16();
        extension_order.push(kind);
        assert!(values.insert(kind, take_vector(&mut extensions)).is_none());
    }

    // 原生后端可能只发送 TLS 1.2 扩展；扩展是否存在仍由完整的类型列表校验
    let u16_list = |kind| {
        values
            .get(&kind)
            .cloned()
            .map(|mut data| u16_values(take_vector(&mut data)))
            .unwrap_or_default()
    };
    let byte_list = |kind| {
        values
            .get(&kind)
            .map(|data| data.slice(1..))
            .unwrap_or_default()
    };
    let mut key_shares = Vec::new();
    if let Some(mut data) = values.get(&51).cloned() {
        let mut shares = take_vector(&mut data);
        while shares.has_remaining() {
            let group = shares.get_u16();
            key_shares.push((group, take_vector(&mut shares).len()));
        }
    }
    let mut alpn = Vec::new();
    if let Some(protocols) = values.get(&16) {
        let mut protocols = protocols.clone();
        let mut protocols = take_vector(&mut protocols);
        while protocols.has_remaining() {
            let length = usize::from(protocols.get_u8());
            alpn.push(String::from_utf8(protocols.split_to(length).to_vec()).unwrap());
        }
    }
    ClientHello {
        legacy_version,
        supported_versions: u16_values(byte_list(43)),
        point_formats: byte_list(11).to_vec(),
        psk_modes: byte_list(45).to_vec(),
        cipher_suites,
        extensions: extension_order,
        groups: u16_list(10),
        signature_algorithms: u16_list(13),
        key_shares,
        alpn,
    }
}

fn take_vector(bytes: &mut Bytes) -> Bytes {
    let length = usize::from(bytes.get_u16());
    bytes.split_to(length)
}

fn u16_values(mut bytes: Bytes) -> Vec<u16> {
    let mut values = Vec::new();
    while bytes.has_remaining() {
        values.push(bytes.get_u16());
    }
    values
}

// 仅用于触发自定义 CA 路径，采集器在 ClientHello 后终止握手
const TEST_CA_PEM: &str = r"-----BEGIN CERTIFICATE-----
MIIBZTCCARegAwIBAgIUbyYrPhPgUSvZ6mOSWi/YkCn0nKIwBQYDK2VwMCgxJjAk
BgNVBAMMHUNvZGV4IFRMUyBmaW5nZXJwcmludCB0ZXN0IENBMB4XDTI2MTAwNTE1
MDY0N1oXDTM2MTAwMjE1MDY0N1owKDEmMCQGA1UEAwwdQ29kZXggVExTIGZpbmdl
cnByaW50IHRlc3QgQ0EwKjAFBgMrZXADIQB2lY3pLKckV8KmxZiVZcmw70+RriQE
oAoUPx49BMSE9KNTMFEwHQYDVR0OBBYEFD8ham5c8YWHeYP7rsqbE/+8Sm1NMB8G
A1UdIwQYMBaAFD8ham5c8YWHeYP7rsqbE/+8Sm1NMA8GA1UdEwEB/wQFMAMBAf8w
BQYDK2VwA0EA15f5R32nl9ke0xc5sNM67R4tUEnxUiG2tUSGYinnFqFgEacMdK37
T/qdob9dovtvUOziymxLs80cMCsGpbFiCw==
-----END CERTIFICATE-----
";
