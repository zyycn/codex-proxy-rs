//! 验证文件日志队列排空与写入失败后的完整性状态

use super::*;
use gateway_core::health::HealthState;
use std::time::Duration;

#[test]
fn file_queue_drains_every_record_before_normal_shutdown() {
    if env::var_os(CHILD_PROCESS_ENV).is_some() {
        with_file_logging(
            PathBuf::from(env::var_os(LOG_DIRECTORY_ENV).unwrap()),
            true,
            || {
                let payload = "x".repeat(16 * 1024);
                for sequence in 0..2000 {
                    tracing::info!(target: REQUEST_DUMP_LOG_TARGET, sequence, payload, "queue record");
                }
            },
        );
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "logging::sink::file_queue_drains_every_record_before_normal_shutdown",
        ])
        .env(CHILD_PROCESS_ENV, "1")
        .env(LOG_DIRECTORY_ENV, directory.path())
        .env("RUST_LOG", "off")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body = read_log_file_set(directory.path(), REQUEST_DUMP_LOG_FILE_PREFIX);
    let mut sequences = body
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["fields"]["sequence"]
                .as_u64()
                .unwrap()
        })
        .collect::<Vec<_>>();
    sequences.sort_unstable();
    assert_eq!(sequences, (0..2000).collect::<Vec<_>>());
}

#[test]
fn write_failure_marks_log_completeness_unhealthy_even_after_writes_recover() {
    if env::var_os(CHILD_PROCESS_ENV).is_some() {
        let directory = PathBuf::from(env::var_os(LOG_DIRECTORY_ENV).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let bundle = runtime
            .block_on(gateway_host::initialize(logging_config(
                directory.clone(),
                true,
            )))
            .unwrap();
        let health = bundle.logging_health_probe();
        assert_eq!(runtime.block_on(health.check()), HealthState::Healthy);
        let blocked = directory.join(format!(
            "{REQUEST_DUMP_LOG_FILE_PREFIX}{}.1.log",
            gateway_core::time::DeploymentTimeZone::default()
                .local(chrono::Utc::now())
                .date_naive()
        ));
        fs::create_dir(&blocked).unwrap();
        let payload = "x".repeat(1024 * 1024);
        tracing::info!(target: REQUEST_DUMP_LOG_TARGET, payload, marker="before-failure");
        tracing::info!(target: REQUEST_DUMP_LOG_TARGET, payload, marker="cannot-write");
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if matches!(health.check().await, HealthState::Unhealthy(_)) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        });
        fs::remove_dir(&blocked).unwrap();
        tracing::info!(target: REQUEST_DUMP_LOG_TARGET, marker="after-recovery");
        drop(bundle);
        assert!(matches!(
            runtime.block_on(health.check()),
            HealthState::Unhealthy(_)
        ));
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env::current_exe().unwrap())
        .args(["--nocapture", "--exact", "logging::sink::write_failure_marks_log_completeness_unhealthy_even_after_writes_recover"])
        .env(CHILD_PROCESS_ENV, "1").env(LOG_DIRECTORY_ENV, directory.path())
        .env("RUST_LOG", "off").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("log completeness compromised"));
    let body = read_log_file_set(directory.path(), REQUEST_DUMP_LOG_FILE_PREFIX);
    assert!(body.contains("before-failure") && body.contains("after-recovery"));
    assert!(!body.contains("cannot-write"));
}

#[cfg(unix)]
#[test]
fn blocked_file_output_keeps_http_and_streaming_responsive_and_reserves_errors() {
    const CHILD: &str = "logging::sink::blocked_file_output_keeps_http_and_streaming_responsive_and_reserves_errors";
    if env::var_os(CHILD_PROCESS_ENV).is_some() {
        let directory = PathBuf::from(env::var_os(LOG_DIRECTORY_ENV).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut config = logging_config(directory.clone(), false);
        config.logging.oauth_recovery = false;
        config.logging.file.max_file_size_mb = 8;
        let bundle = runtime.block_on(gateway_host::initialize(config)).unwrap();
        let health = bundle.logging_health_probe();
        let date = gateway_core::time::DeploymentTimeZone::default()
            .local(chrono::Utc::now())
            .date_naive();
        let fifo = directory.join(format!("{APPLICATION_LOG_FILE_PREFIX}{date}.1.log"));
        assert!(
            Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let (release, released) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            // 请求必须在读端放行前完成，超时让写入阻塞时的测试也能退出
            let _ = released.recv_timeout(Duration::from_secs(3));
            let mut body = String::new();
            fs::File::open(fifo)
                .unwrap()
                .read_to_string(&mut body)
                .unwrap();
            body
        });
        let payload = "x".repeat(8 * 1024 * 1024);
        tracing::info!(target: APPLICATION_LOG_TARGET, payload, "rotate before requests");
        tracing::info!(target: APPLICATION_LOG_TARGET, "block on opening next segment");
        runtime.block_on(async {
            let initial = directory.join(format!("{APPLICATION_LOG_FILE_PREFIX}{date}.log"));
            tokio::time::timeout(Duration::from_secs(2), async {
                while fs::metadata(&initial).unwrap().len() < 8 * 1024 * 1024 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }).await.unwrap();
            use futures::{SinkExt as _, StreamExt as _};
            let app = axum::Router::new()
                .route("/v1/log-test", axum::routing::get(|| async {
                    tracing::error!(target: APPLICATION_LOG_TARGET, path="v1", "critical_after_congestion");
                    "ok"
                }))
                .route("/admin/log-test", axum::routing::get(|| async {
                    tracing::error!(target: APPLICATION_LOG_TARGET, path="admin", "critical_after_congestion");
                    "ok"
                }))
                .route("/v1/stream-test", axum::routing::get(|| async {
                    tracing::error!(target: APPLICATION_LOG_TARGET, path="stream", "critical_after_congestion");
                    axum::response::Sse::new(futures::stream::iter([
                        Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data("first")),
                        Ok(axum::response::sse::Event::default().data("last")),
                    ]))
                }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
            let started = std::time::Instant::now();
            for sequence in 0..5_000 {
                tracing::info!(target: APPLICATION_LOG_TARGET, sequence, "congested regular log");
            }
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            for path in ["/v1/log-test", "/admin/log-test", "/v1/stream-test"] {
                let response = client.get(format!("http://{address}{path}")).send().await.unwrap();
                assert!(response.status().is_success());
                let body = response.text().await.unwrap();
                if path.ends_with("stream-test") {
                    assert!(body.contains("first") && body.contains("last"));
                } else {
                    assert_eq!(body, "ok");
                }
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let websocket_server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                tracing::error!(target: APPLICATION_LOG_TARGET, path="websocket", "critical_after_congestion");
                socket.send(tokio_tungstenite::tungstenite::Message::Text("frame".into())).await.unwrap();
            });
            let stream = tokio::net::TcpStream::connect(address).await.unwrap();
            let (mut websocket, _) = tokio_tungstenite::client_async(format!("ws://{address}/"), stream).await.unwrap();
            assert_eq!(websocket.next().await.unwrap().unwrap().into_text().unwrap(), "frame");
            websocket_server.await.unwrap();
            let elapsed = started.elapsed();
            assert!(elapsed < Duration::from_secs(1), "interfaces waited for logging: {elapsed:?}");
            eprintln!("paused log output: 5000 submissions and HTTP/admin/SSE/WS completed in {elapsed:?}");
            let HealthState::Unhealthy(message) = health.check().await else { panic!("queue overflow must be visible") };
            assert!(message.contains("0 warnings/errors"), "{message}");
            server.abort();
        });
        release.send(()).unwrap();
        drop(bundle);
        let body = reader.join().unwrap();
        assert_eq!(
            body.lines()
                .filter(|line| line.contains("critical_after_congestion"))
                .count(),
            4
        );
        for line in body.lines() {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
        }
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env::current_exe().unwrap())
        .args(["--exact", CHILD, "--nocapture"])
        .env(CHILD_PROCESS_ENV, "1")
        .env(LOG_DIRECTORY_ENV, directory.path())
        .env("RUST_LOG", "off,logging_test_application=info")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
}
