//! 验证服务组装为 HTTPS 代理探测传递自定义 CA

use std::{fs, process::Command};

#[tokio::test]
async fn proxy_probe_should_use_provider_custom_ca_for_https_proxies() {
    use gateway_admin::ports::proxy::ProxyProbe;
    use gateway_core::account::OutboundProxy;
    use gateway_host::proxy_probe::HttpProxyProbe;
    use std::{sync::Arc, time::Duration};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio_rustls::{
        TlsAcceptor,
        rustls::{
            ServerConfig,
            pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _},
        },
    };

    const CHILD_ENV: &str = "CPR_TEST_PROXY_TLS_DIRECTORY";
    let Ok(directory) = std::env::var(CHILD_ENV) else {
        let directory = tempfile::tempdir().unwrap();
        generate_proxy_test_certificates(directory.path());
        // 使用子进程隔离环境变量，避免并行测试读取到临时 CA 配置
        for ca_env in ["CODEX_CA_CERTIFICATE", "SSL_CERT_FILE"] {
            let mut child = Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "bootstrap::server::proxy_probe_should_use_provider_custom_ca_for_https_proxies",
                    "--nocapture",
                ])
                .env(CHILD_ENV, directory.path())
                .env_remove("CODEX_CA_CERTIFICATE")
                .env_remove("SSL_CERT_FILE")
                .env(ca_env, directory.path().join("ca.pem"));
            if ca_env == "CODEX_CA_CERTIFICATE" {
                child.env(
                    "SSL_CERT_FILE",
                    directory.path().join("missing-fallback.pem"),
                );
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "{ca_env}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    };
    provider_openai::ensure_rustls_provider();
    let directory = std::path::Path::new(&directory);
    let certificate = CertificateDer::from_pem_file(directory.join("server.pem")).unwrap();
    let key = PrivateKeyDer::from_pem_file(directory.join("server.key")).unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key)
            .unwrap(),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy =
        OutboundProxy::parse(&format!("https://{}", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut stream = acceptor.accept(socket).await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            assert!(request.len() < 8192);
            request.push(stream.read_u8().await.unwrap());
        }
        assert!(request.starts_with(b"GET http://unresolvable.invalid/ip "));
        let body = "{\"ip\":\"203.0.113.8\"}";
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
    });
    let probe = HttpProxyProbe::new("http://unresolvable.invalid/ip")
        .with_client_builder(provider_openai::build_reqwest_client_with_custom_ca);
    let result = probe.test(&proxy, false).await;
    assert!(result.success, "{}", result.message);
    assert_eq!(result.exit_ip.unwrap().to_string(), "203.0.113.8");
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

fn generate_proxy_test_certificates(directory: &std::path::Path) {
    let openssl = |args: &[&str]| {
        let output = Command::new("openssl")
            .args(args)
            .current_dir(directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    openssl(&[
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        "ca.key",
        "-out",
        "ca.pem",
        "-days",
        "1",
        "-subj",
        "/CN=Proxy Test CA",
    ]);
    openssl(&[
        "req",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        "server.key",
        "-out",
        "server.csr",
        "-subj",
        "/CN=localhost",
    ]);
    fs::write(directory.join("extensions"), "subjectAltName=IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n").unwrap();
    openssl(&[
        "x509",
        "-req",
        "-in",
        "server.csr",
        "-CA",
        "ca.pem",
        "-CAkey",
        "ca.key",
        "-CAcreateserial",
        "-out",
        "server.pem",
        "-days",
        "1",
        "-extfile",
        "extensions",
    ]);
}
