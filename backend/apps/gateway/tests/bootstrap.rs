use std::{fs, process::Command};

use codex_proxy_rs::bootstrap::GatewayConfig;
use gateway_host::LoadableConfig;

const CONFIG_EXAMPLE: &str = include_str!("../../../../deploy/config.example.yaml");
const POSTGRES_PASSWORD: &str = "111111111111111111111111111111111111111111111111";
const REDIS_PASSWORD: &str = "222222222222222222222222222222222222222222222222";
const ADMIN_PASSWORD: &str = "test-admin-password";
const TOPOLOGY_CHILD_ENV: &str = "CPR_TEST_TOPOLOGY_CHILD";

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
        // 使用子进程隔离环境变量，避免并行测试读取到临时 CA 配置。
        for ca_env in ["CODEX_CA_CERTIFICATE", "SSL_CERT_FILE"] {
            let mut child = Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "bootstrap::proxy_probe_should_use_provider_custom_ca_for_https_proxies",
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
    let result = probe.test(&proxy).await;
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

#[test]
fn config_loader_should_load_complete_terminal_example() {
    parse_config(&valid_config()).expect("terminal config example");
}

#[test]
fn config_loader_should_resolve_paths_relative_to_config_file() {
    let (config, _directory) = parse_config(&valid_config()).expect("resolved config");
    let debug = format!("{config:?}");
    assert!(debug.contains(".runtime/data"));
    assert!(debug.contains(".runtime/logs"));
    assert!(debug.contains("frontend/dist"));
}

#[test]
fn config_loader_should_share_resolved_assets_with_system_update() {
    const CHILD_ENV: &str = "CPR_TEST_UPDATE_ASSETS_CHILD";
    let Ok(case) = std::env::var(CHILD_ENV) else {
        // 使用子进程覆盖无环境变量、Docker 路径和相对路径，避免污染并行测试。
        for (case, web_dist) in [
            ("binary", None),
            ("docker", Some("/app/web/dist")),
            ("relative", Some("../custom/dist")),
        ] {
            let mut child = Command::new(std::env::current_exe().expect("test executable"));
            child
                .args([
                    "--exact",
                    "bootstrap::config_loader_should_share_resolved_assets_with_system_update",
                ])
                .env(CHILD_ENV, case)
                .env_remove("CPR_WEB_DIST_DIR");
            if let Some(web_dist) = web_dist {
                child.env("CPR_WEB_DIST_DIR", web_dist);
            }
            let output = child.output().expect("isolated configuration test");
            assert!(
                output.status.success(),
                "{case}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    };
    let mut document = valid_config_document();
    document["api"]["asset_directory"] = serde_json::json!("../web/dist");
    let (config, directory) = parse_config(&document.to_string()).expect("binary configuration");
    let assets = match case.as_str() {
        "docker" => std::path::PathBuf::from("/app/web/dist"),
        "relative" => directory.path().join("deploy/../custom/dist"),
        _ => directory.path().join("deploy/../web/dist"),
    };
    let debug = format!("{config:?}");
    assert!(debug.contains(&format!("asset_directory: {assets:?}")));
    assert!(debug.contains(&format!("web_dist_dir: Some({assets:?})")));
}

#[test]
fn config_loader_should_reject_missing_runtime_data_dir() {
    let config = valid_config().replace("  runtime_data_dir: '../.runtime/data'\n", "");

    assert!(parse_config(&config).is_err());
}

#[test]
fn config_loader_should_inject_connection_passwords_into_urls() {
    parse_config(&valid_config()).expect("Store validates password injection into both URLs");
}

#[test]
fn config_loader_should_apply_only_explicit_topology_overrides() {
    let invalid = valid_config()
        .replace("host: '127.0.0.1'", "host: ''")
        .replace("port: 8080", "port: 0")
        .replace(
            "url: 'postgres://codex_proxy@127.0.0.1:5432/codex_proxy'",
            "url: 'invalid-postgres-url'",
        )
        .replace("url: 'redis://127.0.0.1:6379/'", "url: 'invalid-redis-url'")
        .replace(POSTGRES_PASSWORD, "invalid-postgres-password")
        .replace(REDIS_PASSWORD, "invalid-redis-password");
    if std::env::var_os(TOPOLOGY_CHILD_ENV).is_some() {
        parse_config(&invalid).expect("explicit package-owned environment overrides");
        return;
    }
    assert!(parse_config(&invalid).is_err());
    let status = Command::new(std::env::current_exe().expect("current test executable"))
        .args([
            "--exact",
            "bootstrap::config_loader_should_apply_only_explicit_topology_overrides",
        ])
        .env(TOPOLOGY_CHILD_ENV, "1")
        .env("CPR_SERVER_HOST", "127.0.0.1")
        .env("CPR_SERVER_PORT", "8080")
        .env(
            "CPR_DATABASE_URL",
            "postgres://codex_proxy@127.0.0.1:5432/codex_proxy",
        )
        .env("CPR_REDIS_URL", "redis://127.0.0.1:6379/")
        .env("CPR_DATABASE_PASSWORD", POSTGRES_PASSWORD)
        .env("CPR_REDIS_PASSWORD", REDIS_PASSWORD)
        .status()
        .expect("run isolated environment override test");
    assert!(status.success());
}

#[test]
fn bootstrap_config_debug_should_redact_all_passwords() {
    let (config, _directory) = parse_config(&valid_config()).expect("config");
    let debug = format!("{config:?}");
    assert!(!debug.contains(POSTGRES_PASSWORD));
    assert!(!debug.contains(REDIS_PASSWORD));
    assert!(!debug.contains(ADMIN_PASSWORD));
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn config_loader_should_ignore_unknown_fields_in_configuration_sections() {
    let mut document = valid_config_document();
    document["host"]["system_update"] = serde_json::json!({});
    document["store"]["pool"] = serde_json::json!({});
    for path in [
        "",
        "/host",
        "/host/listen",
        "/host/logging",
        "/host/logging/file",
        "/host/system_update",
        "/store",
        "/store/database",
        "/store/redis",
        "/store/pool",
        "/admin",
        "/client",
        "/api",
        "/openai",
    ] {
        let mut extended = document.clone();
        extended.pointer_mut(path).expect("configuration section")["unused_setting"] =
            serde_json::json!({"nested": [true, null, "ignored"]});
        parse_config(&extended.to_string())
            .unwrap_or_else(|error| panic!("unknown field in {path}: {error}"));
    }
}

#[test]
fn config_loader_should_ignore_removed_tls_and_fingerprint_sections() {
    let mut document = valid_config_document();
    document["openai"]["tls"] = serde_json::json!({});
    document["openai"]["fingerprint"] = serde_json::json!({"browser": "removed"});
    parse_config(&document.to_string()).expect("removed provider settings are ignored");
}

#[test]
fn config_loader_should_reject_invalid_known_fields_alongside_unknown_fields() {
    for port in [serde_json::json!(0), serde_json::json!("not-a-port")] {
        let mut document = valid_config_document();
        document["host"]["listen"]["unused_setting"] = serde_json::json!(true);
        document["host"]["listen"]["port"] = port;
        assert_rejected(document.to_string());
    }
}

#[test]
fn config_loader_should_report_startup_configuration_diagnostics() {
    const CHILD_ENV: &str = "CPR_TEST_CONFIG_DIAGNOSTICS_CHILD";
    const UNUSED_SECRET: &str = "unused-value-must-not-appear-in-diagnostics";
    if std::env::var_os(CHILD_ENV).is_some() {
        gateway_host::load_config::<GatewayConfig>().expect("startup configuration");
        return;
    }
    for case in ["normal", "unused", "missing"] {
        let mut document = valid_config_document();
        if case == "unused" {
            document["openai"]["wire_profile"]["location"] = serde_json::json!(UNUSED_SECRET);
        } else if case == "missing" {
            document["host"]["listen"]
                .as_object_mut()
                .unwrap()
                .remove("port");
        }
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("deploy")).unwrap();
        fs::write(
            directory.path().join("deploy/config.yaml"),
            document.to_string(),
        )
        .unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "bootstrap::config_loader_should_report_startup_configuration_diagnostics",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .current_dir(directory.path())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.success(),
            case != "missing",
            "{case}: {stderr}"
        );
        match case {
            "unused" => assert!(
                stderr.contains("openai.wire_profile") && stderr.contains("已忽略"),
                "{stderr}"
            ),
            "missing" => assert!(stderr.contains("host.listen.port"), "{stderr}"),
            _ => assert!(!stderr.contains("警告"), "{stderr}"),
        }
        assert!(!stderr.contains(UNUSED_SECRET), "{stderr}");
        assert!(!stderr.contains("services"), "{stderr}");
    }
}

#[test]
fn config_loader_should_reject_missing_explicit_fields() {
    assert_rejected(valid_config().replace("  request_id_header: 'x-request-id'\n", ""));
}

#[test]
fn config_loader_should_allow_missing_provider_configuration() {
    let mut document = valid_config_document();
    document.as_object_mut().unwrap().remove("openai");
    document.as_object_mut().unwrap().remove("xai");
    parse_config(&document.to_string()).expect("provider defaults require no YAML identity");
}

#[test]
fn config_loader_should_ignore_removed_provider_identity_sections() {
    let mut document = valid_config_document();
    document["openai"]["wire_profile"] = serde_json::json!({"codex_version":"legacy-invalid"});
    document["xai"] = serde_json::json!({"wire_profile": {"client_identifier": "legacy-client"}});
    let (config, _directory) =
        parse_config(&document.to_string()).expect("removed identities ignored");
    let debug = format!("{config:?}");
    assert!(!debug.contains("legacy-invalid"));
    assert!(!debug.contains("legacy-client"));
}

#[test]
fn config_loader_should_reject_unsupported_schema_version() {
    assert_rejected(valid_config().replace("schema_version: 1", "schema_version: 2"));
}

#[test]
fn config_loader_should_reject_embedded_database_password() {
    assert_rejected(valid_config().replace(
        "postgres://codex_proxy@127.0.0.1:5432/codex_proxy",
        "postgres://codex_proxy:embedded@127.0.0.1:5432/codex_proxy",
    ));
}

#[test]
fn config_loader_should_reject_non_hex_postgres_password() {
    assert_rejected(valid_config().replace(POSTGRES_PASSWORD, &"g".repeat(48)));
}

#[test]
fn config_loader_should_reject_wrong_length_redis_password() {
    assert_rejected(valid_config().replace(REDIS_PASSWORD, "1234"));
}

#[test]
fn config_loader_should_reject_weak_admin_password() {
    assert_rejected(valid_config().replace(ADMIN_PASSWORD, "password"));
}

#[test]
fn config_loader_should_reject_admin_password_with_compose_interpolation() {
    assert_rejected(valid_config().replace(ADMIN_PASSWORD, "unsafe$password"));
}

#[test]
fn config_loader_should_reject_zero_client_session_ttl() {
    let mut config = valid_config_document();
    *config
        .pointer_mut("/client/session_ttl_minutes")
        .expect("example client session TTL") = serde_json::json!(0);
    assert_rejected(config.to_string());
}

#[test]
fn config_loader_should_default_missing_client_section() {
    let mut config = valid_config_document();
    config
        .as_object_mut()
        .expect("example config mapping")
        .remove("client")
        .expect("example client section");
    parse_config(&config.to_string()).expect("client defaults when the section is omitted");
}

#[test]
fn config_loader_should_reject_missing_client_session_ttl() {
    let mut config = valid_config_document();
    config["client"]
        .as_object_mut()
        .expect("example client mapping")
        .remove("session_ttl_minutes")
        .expect("example client session TTL");
    assert_rejected(config.to_string());
}

#[test]
fn config_loader_should_reject_disabled_all_log_outputs() {
    assert_rejected(
        valid_config()
            .replace("stdout: true", "stdout: false")
            .replace("enabled: true", "enabled: false"),
    );
}

#[test]
fn config_loader_should_reject_zero_server_port() {
    assert_rejected(valid_config().replace("port: 8080", "port: 0"));
}

#[test]
fn config_loader_should_validate_openai_residency() {
    let mut document = valid_config_document();
    document["openai"]["residency"] = serde_json::json!("invalid");
    assert_rejected(document.to_string());
}

fn assert_rejected(config: String) {
    assert!(parse_config(&config).is_err());
}

fn valid_config() -> String {
    CONFIG_EXAMPLE
        .replacen(
            "password: &postgres_password ''",
            &format!("password: &postgres_password '{POSTGRES_PASSWORD}'"),
            1,
        )
        .replacen(
            "password: &redis_password ''",
            &format!("password: &redis_password '{REDIS_PASSWORD}'"),
            1,
        )
        .replace(
            "default_password: ''",
            &format!("default_password: '{ADMIN_PASSWORD}'"),
        )
}

fn valid_config_document() -> serde_json::Value {
    // 按字段修改样例，避免注释或排版变化让测试输入悄悄失效；JSON 仍可由 YAML 文件入口加载。
    config::Config::builder()
        .add_source(config::File::from_str(
            &valid_config(),
            config::FileFormat::Yaml,
        ))
        .build()
        .and_then(config::Config::try_deserialize)
        .expect("example config document")
}

fn parse_config(config: &str) -> Result<(GatewayConfig, tempfile::TempDir), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let deploy = directory.path().join("deploy");
    fs::create_dir(&deploy).map_err(|error| error.to_string())?;
    let path = deploy.join("config.yaml");
    fs::write(&path, config).map_err(|error| error.to_string())?;
    let mut config = config::Config::builder()
        .add_source(config::File::from(path).required(true))
        .build()
        .and_then(config::Config::try_deserialize::<GatewayConfig>)
        .map_err(|error| error.to_string())?;
    config
        .resolve_and_validate(&deploy)
        .map_err(|error| error.to_string())?;
    Ok((config, directory))
}
