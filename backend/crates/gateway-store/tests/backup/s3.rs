//! 验证 S3 分片完整读取、并发上限与取消和失败时的请求回收

use std::{io, time::Duration};

use chrono::Utc;
use gateway_admin::model::backup::{BackupError, BackupObjectMetadata, BackupStorageConfig, code};
use gateway_admin::ports::backup::{BackupObjectStorePort, UploadObjectRequest};
use gateway_core::lifecycle::CancellationToken;
use gateway_store::backup::s3::S3ObjectStoreAdapter;
use secrecy::SecretString;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

const PART_SIZE_ABOVE_TOKIO_DEFAULT: usize = 2 * 1024 * 1024 + 1;
const MULTIPART_SIZE: usize = 16 * 1024 * 1024;

#[tokio::test]
async fn upload_file_sends_full_part_beyond_tokio_default_buffer_limit() {
    let directory = tempfile::tempdir().expect("create temporary directory");
    let source = directory.path().join("archive.dump");
    let expected = vec![b'x'; PART_SIZE_ABOVE_TOKIO_DEFAULT];
    tokio::fs::write(&source, &expected)
        .await
        .expect("write temporary archive");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind multipart probe server");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("read listener address")
    );
    let server = tokio::spawn(serve_multipart_probe(listener));
    let storage = BackupStorageConfig {
        storage_revision: 1,
        endpoint,
        region: "auto".to_owned(),
        bucket: "backup-bucket".to_owned(),
        access_key_id: "test-access-key".to_owned(),
        secret_access_key: SecretString::from("test-secret-key"),
        prefix: "backups".to_owned(),
        force_path_style: true,
    };
    let request = UploadObjectRequest {
        object_key: "backups/archive.dump".to_owned(),
        source,
        metadata: BackupObjectMetadata::new(
            "backup_test".to_owned(),
            "a".repeat(64),
            Utc::now(),
            PART_SIZE_ABOVE_TOKIO_DEFAULT as u64,
        ),
        cancellation: CancellationToken::new(),
    };

    S3ObjectStoreAdapter::new()
        .upload_file(&storage, request)
        .await
        .expect("upload multipart archive");
    let uploaded_len = server
        .await
        .expect("multipart probe task did not panic")
        .expect("serve multipart probe");

    assert_eq!(uploaded_len, PART_SIZE_ABOVE_TOKIO_DEFAULT);
}

#[tokio::test]
async fn cancelled_upload_does_not_retry_a_pending_part_after_returning() {
    let probe = start_upload(MULTIPART_SIZE).await;
    let (mut part, request, _) = accept_request(&probe.listener).await;
    assert!(request.starts_with("PUT "));

    probe.cancellation.cancel();
    let (mut abort, request, _) = accept_request(&probe.listener).await;
    assert!(request.starts_with("DELETE ") && request.contains("uploadId="));
    respond(&mut abort, &xml_response("")).await;
    let error = tokio::time::timeout(Duration::from_secs(3), probe.upload)
        .await
        .expect("cancelled upload returns without waiting for the part response")
        .expect("upload task did not panic")
        .expect_err("upload was cancelled");
    assert_eq!(error.code(), code::CANCELLED);

    assert_no_part_retry(&probe.listener, &mut part).await;
}

#[tokio::test]
async fn a_failed_part_stops_other_pending_parts_before_upload_returns() {
    let probe = start_upload(MULTIPART_SIZE * 2).await;
    let (mut failed, request, _) = accept_request(&probe.listener).await;
    assert!(request.starts_with("PUT "));
    let (mut pending, request, _) = accept_request(&probe.listener).await;
    assert!(request.starts_with("PUT "));
    respond(
        &mut failed,
        &error_response("403 Forbidden", "AccessDenied"),
    )
    .await;

    // 另一分片仍未收到响应，首次失败必须立即撤销它并进入 multipart 清理
    let (mut abort, request, _) = accept_request(&probe.listener).await;
    assert!(request.starts_with("DELETE ") && request.contains("uploadId="));
    respond(&mut abort, &xml_response("")).await;
    let error = tokio::time::timeout(Duration::from_secs(3), probe.upload)
        .await
        .expect("failed upload returns without waiting for the other part")
        .expect("upload task did not panic")
        .expect_err("part upload failed");
    assert_eq!(error.code(), code::S3_PERMISSION_DENIED);

    assert_no_part_retry(&probe.listener, &mut pending).await;
}

#[tokio::test]
async fn dropping_the_upload_future_stops_pending_part_retries() {
    let probe = start_upload(MULTIPART_SIZE).await;
    let (mut part, request, _) = accept_request(&probe.listener).await;
    assert!(request.starts_with("PUT "));

    probe.upload.abort();
    assert!(
        probe
            .upload
            .await
            .expect_err("parent task was aborted")
            .is_cancelled()
    );

    assert_no_part_retry(&probe.listener, &mut part).await;
}

#[tokio::test]
async fn multipart_upload_bounds_concurrency_and_completes_parts_in_order() {
    let probe = start_upload(MULTIPART_SIZE * 4 + 1).await;
    let mut pending = Vec::new();
    for _ in 0..4 {
        let (part, request, body) = accept_request(&probe.listener).await;
        assert!(request.starts_with("PUT "));
        assert_eq!(body.len(), MULTIPART_SIZE);
        pending.push(part);
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(150), probe.listener.accept())
            .await
            .is_err(),
        "a fifth part must wait for one of the four active uploads"
    );
    respond(&mut pending.pop().unwrap(), part_response()).await;
    let (mut last, request, body) = accept_request(&probe.listener).await;
    assert!(request.starts_with("PUT ") && request.contains("partNumber=5"));
    assert_eq!(body.len(), 1);
    respond(&mut last, part_response()).await;
    for mut part in pending.into_iter().rev() {
        respond(&mut part, part_response()).await;
    }
    let (mut complete, request, body) = accept_request(&probe.listener).await;
    assert!(request.starts_with("POST ") && request.contains("uploadId="));
    let body = String::from_utf8(body).unwrap();
    let positions: Vec<_> = (1..=5)
        .map(|part| {
            body.find(&format!("<PartNumber>{part}</PartNumber>"))
                .unwrap()
        })
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    respond(
        &mut complete,
        &xml_response(
            "<CompleteMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><ETag>\"probe-part\"</ETag></CompleteMultipartUploadResult>",
        ),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), probe.upload)
        .await
        .expect("multipart upload completes")
        .expect("upload task did not panic")
        .expect("all parts uploaded");
}

struct UploadProbe {
    listener: TcpListener,
    cancellation: CancellationToken,
    upload: tokio::task::JoinHandle<Result<(), BackupError>>,
    _directory: tempfile::TempDir,
}

async fn start_upload(size: usize) -> UploadProbe {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("archive.dump");
    let file = tokio::fs::File::create(&source).await.unwrap();
    file.set_len(size as u64).await.unwrap();
    drop(file);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let storage = BackupStorageConfig {
        storage_revision: 1,
        endpoint: format!("http://{}", listener.local_addr().unwrap()),
        region: "auto".into(),
        bucket: "backup-bucket".into(),
        access_key_id: "test-access-key".into(),
        secret_access_key: SecretString::from("test-secret-key"),
        prefix: "backups".into(),
        force_path_style: true,
    };
    let cancellation = CancellationToken::new();
    let request = UploadObjectRequest {
        object_key: "backups/archive.dump".into(),
        source,
        metadata: BackupObjectMetadata::new(
            "backup_test".into(),
            "a".repeat(64),
            Utc::now(),
            size as u64,
        ),
        cancellation: cancellation.clone(),
    };
    let upload = tokio::spawn(async move {
        S3ObjectStoreAdapter::new()
            .upload_file(&storage, request)
            .await
    });
    let (mut create, request, _) = accept_request(&listener).await;
    assert!(request.starts_with("POST ") && request.contains("uploads"));
    respond(
        &mut create,
        &xml_response(
            "<InitiateMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><UploadId>probe-upload</UploadId></InitiateMultipartUploadResult>",
        ),
    )
    .await;
    UploadProbe {
        listener,
        cancellation,
        upload,
        _directory: directory,
    }
}

async fn accept_request(listener: &TcpListener) -> (TcpStream, String, Vec<u8>) {
    tokio::time::timeout(Duration::from_secs(3), async {
        let (mut stream, _) = listener.accept().await.expect("accept S3 request");
        let (request, body) = read_http_request(&mut stream)
            .await
            .expect("read S3 request");
        (stream, request, body)
    })
    .await
    .expect("expected S3 request arrives without waiting for another part")
}

async fn respond(stream: &mut TcpStream, response: &str) {
    stream.write_all(response.as_bytes()).await.unwrap();
    stream.shutdown().await.unwrap();
}

async fn assert_no_part_retry(listener: &TcpListener, pending: &mut TcpStream) {
    // 旧请求可能已被关闭，写入失败是正常撤销结果；仍活动的 SDK 调用收到 503 后会重试
    let _ = pending
        .write_all(error_response("503 Service Unavailable", "SlowDown").as_bytes())
        .await;
    let _ = pending.shutdown().await;
    assert!(
        tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .is_err(),
        "a retired part must not issue another UploadPart request"
    );
}

fn part_response() -> &'static str {
    "HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\netag: \"probe-part\"\r\n\r\n"
}

fn error_response(status: &str, code: &str) -> String {
    let body = format!("<Error><Code>{code}</Code><Message>probe error</Message></Error>");
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/xml\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

async fn serve_multipart_probe(listener: TcpListener) -> io::Result<usize> {
    let mut uploaded_len = None;
    for _ in 0..3 {
        let (mut stream, _) = listener.accept().await?;
        let (request, body) = read_http_request(&mut stream).await?;
        let response = if request.starts_with("POST ") && request.contains("uploads") {
            xml_response(
                "<InitiateMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><UploadId>probe-upload</UploadId></InitiateMultipartUploadResult>",
            )
        } else if request.starts_with("PUT ") && request.contains("partNumber=") {
            uploaded_len = Some(body.len());
            "HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\netag: \"probe-part\"\r\n\r\n".to_owned()
        } else if request.starts_with("POST ") && request.contains("uploadId=") {
            xml_response(
                "<CompleteMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><ETag>\"probe-part\"</ETag></CompleteMultipartUploadResult>",
            )
        } else {
            return Err(io::Error::other("unexpected multipart probe request"));
        };
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await?;
    }
    uploaded_len.ok_or_else(|| io::Error::other("multipart probe did not receive an upload part"))
}

async fn read_http_request(stream: &mut TcpStream) -> io::Result<(String, Vec<u8>)> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let mut chunk = [0_u8; 8 * 1024];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "multipart probe request ended before headers",
            ));
        }
        bytes.extend_from_slice(&chunk[..read]);
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
    let content_len = headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or_default();
    if headers
        .to_ascii_lowercase()
        .contains("expect: 100-continue")
    {
        stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await?;
    }
    while bytes.len() < header_end + content_len {
        let mut chunk = [0_u8; 8 * 1024];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "multipart probe request ended before body",
            ));
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok((
        headers,
        bytes[header_end..header_end + content_len].to_vec(),
    ))
}

fn xml_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/xml\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn connection_probe_requires_successful_final_delete() {
    for delete_allowed in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let storage = BackupStorageConfig {
            storage_revision: 1,
            endpoint: format!("http://{}", listener.local_addr().unwrap()),
            region: "auto".into(),
            bucket: "backup-bucket".into(),
            access_key_id: "test-access-key".into(),
            secret_access_key: SecretString::from("test-secret-key"),
            prefix: "backups".into(),
            force_path_style: true,
        };
        let probe =
            tokio::spawn(
                async move { S3ObjectStoreAdapter::new().test_connection(&storage).await },
            );
        let (mut put, request, body) = accept_request(&listener).await;
        assert!(request.starts_with("PUT "));
        respond(&mut put, &xml_response("")).await;
        let (mut head, request, _) = accept_request(&listener).await;
        assert!(request.starts_with("HEAD "));
        respond(&mut head, "HTTP/1.1 200 OK\r\ncontent-length: 64\r\nx-amz-meta-probe: backup-connection-test\r\nconnection: close\r\n\r\n").await;
        let (mut get, request, _) = accept_request(&listener).await;
        assert!(request.starts_with("GET "));
        get.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        get.write_all(&body).await.unwrap();
        get.shutdown().await.unwrap();
        let (mut delete, request, _) = accept_request(&listener).await;
        assert!(request.starts_with("DELETE "));
        respond(
            &mut delete,
            &if delete_allowed {
                xml_response("")
            } else {
                error_response("403 Forbidden", "AccessDenied")
            },
        )
        .await;
        let result = tokio::time::timeout(Duration::from_secs(3), probe)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(result.ok, delete_allowed);
        assert_eq!(result.stage, "deleteObject");
        assert_eq!(
            result.code,
            (!delete_allowed).then_some(code::S3_PERMISSION_DENIED)
        );
    }
}

#[tokio::test]
async fn failed_multipart_retains_abort_failure_without_changing_primary_code() {
    for cancelled in [false, true] {
        let probe = start_upload(MULTIPART_SIZE).await;
        let (mut part, request, _) = accept_request(&probe.listener).await;
        assert!(request.starts_with("PUT "));
        if cancelled {
            probe.cancellation.cancel();
        } else {
            respond(&mut part, &error_response("403 Forbidden", "AccessDenied")).await;
        }
        let (mut abort, request, _) = accept_request(&probe.listener).await;
        assert!(request.starts_with("DELETE ") && request.contains("uploadId="));
        respond(&mut abort, &error_response("403 Forbidden", "AccessDenied")).await;
        let error = tokio::time::timeout(Duration::from_secs(3), probe.upload)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(
            error.code(),
            if cancelled {
                code::CANCELLED
            } else {
                code::S3_PERMISSION_DENIED
            }
        );
        if cancelled {
            assert!(error.message().contains("上传已取消"));
        }
        assert!(error.message().contains("AbortMultipartUpload"));
        assert!(error.message().contains(code::S3_PERMISSION_DENIED));
        assert!(error.message().contains("未安排自动重试"));
        assert!(!error.message().contains("probe-upload"));
        assert!(!error.message().contains("probe error"));
    }
}
