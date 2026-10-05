//! 验证 HTTP Range 的 ETag、精确范围与取消传播

use provider_openai::transport::profile::platform_release::range::RemoteFile;
use std::io::{self, Read};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method},
};

#[tokio::test]
async fn ranged_reads_require_one_etag_and_the_exact_requested_range() {
    for (etag, content_range, fails) in [
        ("\"first\"", "bytes 0-7/8", false),
        ("\"second\"", "bytes 0-7/8", true),
        ("\"first\"", "bytes 1-8/9", true),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("range", "bytes=0-0"))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("etag", "\"first\"")
                    .insert_header("content-range", "bytes 0-0/8")
                    .set_body_bytes(vec![0]),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(header("range", "bytes=0-7"))
            .and(header("if-match", "\"first\""))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("etag", etag)
                    .insert_header("content-range", content_range)
                    .set_body_bytes(vec![1; 8]),
            )
            .mount(&server)
            .await;
        let mut reader = RemoteFile::open(
            reqwest::Client::new(),
            server.uri(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let result = tokio::task::spawn_blocking(move || reader.read_to_end(&mut Vec::new()))
            .await
            .unwrap();
        assert_eq!(result.is_err(), fails);
    }
}

#[tokio::test]
async fn cancelled_reads_should_terminate_standard_read_helpers() {
    let cancellation = CancellationToken::new();
    let (_server, mut reader) = open_reader(cancellation.clone()).await;
    cancellation.cancel();

    tokio::task::spawn_blocking(move || {
        // 先验证单次 read，回归时直接失败，避免 read_exact 的自动重试挂住测试进程
        assert_eq!(
            reader.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::Other,
        );
        assert_eq!(
            reader.read_exact(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::Other,
        );
        assert_eq!(
            reader.read_to_end(&mut Vec::new()).unwrap_err().kind(),
            io::ErrorKind::Other,
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancellation_during_range_request_should_terminate_blocking_read() {
    let cancellation = CancellationToken::new();
    let _cancel_on_drop = cancellation.clone().drop_guard();
    let (server, mut reader) = open_reader(cancellation.clone()).await;
    let requested = Arc::new(Notify::new());
    let request_started = Arc::clone(&requested);
    Mock::given(method("GET"))
        .and(header("range", "bytes=0-7"))
        .and(header("if-match", "\"first\""))
        .respond_with(move |_: &wiremock::Request| {
            request_started.notify_one();
            ResponseTemplate::new(206)
                .insert_header("etag", "\"first\"")
                .insert_header("content-range", "bytes 0-7/8")
                .set_body_bytes(vec![1; 8])
                .set_delay(Duration::from_secs(60))
        })
        .expect(1)
        .mount(&server)
        .await;
    let read = tokio::task::spawn_blocking(move || reader.read(&mut [0]));
    tokio::time::timeout(Duration::from_secs(5), requested.notified())
        .await
        .expect("range request started");
    cancellation.cancel();

    let error = tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("cancelled blocking read completed")
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);
}

async fn open_reader(cancellation: CancellationToken) -> (MockServer, RemoteFile) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header("range", "bytes=0-0"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("etag", "\"first\"")
                .insert_header("content-range", "bytes 0-0/8")
                .set_body_bytes(vec![0]),
        )
        .expect(1)
        .mount(&server)
        .await;
    let reader = RemoteFile::open(reqwest::Client::new(), server.uri(), cancellation)
        .await
        .unwrap();
    (server, reader)
}
