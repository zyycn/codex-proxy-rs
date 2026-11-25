//! 分段下载必须匹配对象版本、范围和长度，完整下载核对清单摘要

use crate::profile::archive::{RemoteZip, download};
use sha2::{Digest, Sha512};
use std::io::Read;
use wiremock::{Mock, MockServer, Request, ResponseTemplate, matchers::any};

pub(super) async fn serve(body: Vec<u8>, fault: &'static str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(move |request: &Request| {
            let response = ResponseTemplate::new(200).insert_header("etag", "\"artifact-1\"");
            if request.method.as_str() == "HEAD" {
                return response.insert_header("content-length", body.len().to_string());
            }
            if let Some(range) = request.headers.get("range") {
                assert_eq!(request.headers.get("if-match").unwrap(), "\"artifact-1\"");
                let (start, end) = range
                    .to_str()
                    .unwrap()
                    .strip_prefix("bytes=")
                    .unwrap()
                    .split_once('-')
                    .unwrap();
                let start: usize = start.parse().unwrap();
                let end: usize = end.parse().unwrap();
                let mut response =
                    ResponseTemplate::new(if fault == "ignored" { 200 } else { 206 })
                        .insert_header(
                            "etag",
                            if fault == "changed" {
                                "\"artifact-2\""
                            } else {
                                "\"artifact-1\""
                            },
                        )
                        .insert_header(
                            "content-range",
                            format!("bytes {start}-{end}/{}", body.len()),
                        );
                if fault == "range" {
                    response = response.insert_header("content-range", "bytes 0-1/2");
                }
                return response.set_body_bytes(
                    body[start..=if fault == "truncated" { end - 1 } else { end }].to_vec(),
                );
            }
            response.set_body_bytes(body.clone())
        })
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn zip_reads_only_members_from_a_consistent_object() {
    let server = serve(super::artifact::desktop_zip("2.35.1"), "").await;
    let client = reqwest::Client::new();
    let zip = RemoteZip::open(&client, format!("{}/Antigravity.zip", server.uri()))
        .await
        .unwrap();
    let asar = zip
        .read("Antigravity.app/Contents/Resources/app.asar", 1024)
        .await
        .unwrap();
    assert_eq!(
        crate::profile::artifact::asar_version(&asar).unwrap(),
        "2.35.1"
    );
    assert!(
        zip.read("Antigravity.app/Contents/Resources/app.asar", 1)
            .await
            .is_err()
    );
    assert!(zip.read("absent", 1024).await.is_err());
}

#[tokio::test]
async fn zip_rejects_ignored_changed_invalid_or_truncated_ranges() {
    for fault in ["ignored", "changed", "range", "truncated"] {
        let server = serve(super::artifact::desktop_zip("2.35.1"), fault).await;
        assert!(
            RemoteZip::open(&reqwest::Client::new(), server.uri())
                .await
                .is_err(),
            "{fault}"
        );
    }
    let mut body = super::artifact::desktop_zip("2.35.1");
    let offset = body.len() - 22;
    body[offset + 8..offset + 12].fill(0xff);
    let server = serve(body, "").await;
    assert!(
        RemoteZip::open(&reqwest::Client::new(), server.uri())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn complete_download_requires_the_manifest_digest() {
    let body = b"official artifact fixture";
    let server = serve(body.to_vec(), "").await;
    let client = reqwest::Client::new();
    let digest = hex::encode(Sha512::digest(body));
    let file = download(&client, &server.uri(), Some(&digest))
        .await
        .unwrap();
    assert_eq!(file.metadata().unwrap().len(), body.len() as u64);
    assert!(
        download(&client, &server.uri(), Some(&"0".repeat(128)))
            .await
            .is_err()
    );
}

#[test]
fn cancelled_parser_stops_reading_its_temporary_file() {
    let (mut file, guard) =
        crate::profile::archive::CancellableFile::new(tempfile::tempfile().unwrap());
    assert_eq!(file.read(&mut [0]).unwrap(), 0);
    drop(guard);
    assert!(file.read(&mut [0]).is_err());
}
