use provider_openai::transport::profile::platform_release::range::RemoteFile;
use std::io::Read;
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
