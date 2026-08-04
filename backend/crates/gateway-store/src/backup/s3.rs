//! S3 兼容对象存储适配器（含 Cloudflare R2）。
//!
//! 单个 `BackupObjectStorePort` 实现：方法接受已经校验的存储配置，内部按
//! `storage_revision` 缓存 SDK client。上传采用分片流式读取，内存占用有界；
//! 取消时主动 `AbortMultipartUpload`。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use aws_credential_types::Credentials;
use aws_sdk_s3::config::{BehaviorVersion, Builder as S3ConfigBuilder, Region};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use aws_sdk_s3::{Client as S3Client, presigning::PresigningConfig};
use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use aws_smithy_runtime_api::client::result::SdkError;
use aws_smithy_types::error::metadata::ProvideErrorMetadata;
use aws_smithy_types::retry::RetryConfig;
use aws_smithy_types::timeout::TimeoutConfig;
use chrono::{DateTime, Utc};
use secrecy::ExposeSecret as _;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, SeekFrom};
use tokio::sync::Semaphore;
use uuid::Uuid;

use gateway_admin::model::backup::{
    BackupError, BackupObjectMetadata, BackupStorageConfig, ConnectionTestResult,
    ConnectionTestStage, code,
};
use gateway_admin::ports::backup::{BackupObjectStorePort, UploadObjectRequest};

/// 分片大小：16 MiB。
const PART_SIZE: usize = 16 * 1024 * 1024;
/// 并发上传分片数；最大额外内存约为 `PART_SIZE * 并发数`。
const CONCURRENCY: usize = 4;
/// SDK 单次请求超时。
const OPERATION_TIMEOUT: Duration = Duration::from_secs(300);

/// 探针对象的固定 metadata 值。
const PROBE_METADATA_KEY: &str = "probe";
const PROBE_METADATA_VALUE: &str = "backup-connection-test";

/// S3 兼容对象存储适配器。
pub struct S3ObjectStoreAdapter {
    clients: Mutex<HashMap<u64, S3Client>>,
}

impl Default for S3ObjectStoreAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl S3ObjectStoreAdapter {
    /// 创建适配器；client 按 `storage_revision` 惰性构建并缓存。
    #[must_use]
    pub fn new() -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
        }
    }

    fn client(&self, config: &BackupStorageConfig) -> S3Client {
        if let Some(client) = self
            .clients
            .lock()
            .ok()
            .and_then(|guard| guard.get(&config.storage_revision).cloned())
        {
            return client;
        }
        let client = build_client(config);
        if let Ok(mut guard) = self.clients.lock() {
            guard.insert(config.storage_revision, client.clone());
        }
        client
    }
}

fn build_client(config: &BackupStorageConfig) -> S3Client {
    let retry = RetryConfig::standard().with_max_attempts(3);
    let timeout = TimeoutConfig::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(OPERATION_TIMEOUT)
        .build();
    let s3_config = S3ConfigBuilder::new()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(config.region.clone()))
        .endpoint_url(&config.endpoint)
        .force_path_style(config.force_path_style)
        .credentials_provider(Credentials::new(
            config.access_key_id.clone(),
            config.secret_access_key.expose_secret().to_string(),
            None,
            None,
            "static",
        ))
        .retry_config(retry)
        .timeout_config(timeout)
        .build();
    S3Client::from_conf(s3_config)
}

#[async_trait::async_trait]
impl BackupObjectStorePort for S3ObjectStoreAdapter {
    async fn test_connection(
        &self,
        config: &BackupStorageConfig,
    ) -> Result<ConnectionTestResult, BackupError> {
        let client = self.client(config);
        let probe_key = format!(
            "{}/.probe/{}",
            config.prefix.trim_end_matches('/'),
            Uuid::new_v4()
        );
        let body: Vec<u8> = (0_u8..64).collect();

        if let Err(error) = client
            .put_object()
            .bucket(&config.bucket)
            .key(&probe_key)
            .body(ByteStream::from(body.clone()))
            .metadata(PROBE_METADATA_KEY, PROBE_METADATA_VALUE)
            .send()
            .await
        {
            let _ = best_effort_delete(&client, config, &probe_key).await;
            return Ok(failed(ConnectionTestStage::PutObject, &error));
        }

        match client
            .head_object()
            .bucket(&config.bucket)
            .key(&probe_key)
            .send()
            .await
        {
            Ok(head) => {
                let length_matches = head.content_length() == Some(body.len() as i64);
                let metadata_matches = head
                    .metadata()
                    .and_then(|metadata| metadata.get(PROBE_METADATA_KEY))
                    .is_some_and(|value| value == PROBE_METADATA_VALUE);
                if !length_matches || !metadata_matches {
                    let _ = best_effort_delete(&client, config, &probe_key).await;
                    return Ok(ConnectionTestResult {
                        ok: false,
                        stage: ConnectionTestStage::HeadObject.as_str(),
                        code: Some(code::UPSTREAM_FAILURE),
                        message: "探针对象校验不一致".to_owned(),
                    });
                }
            }
            Err(error) => {
                let _ = best_effort_delete(&client, config, &probe_key).await;
                return Ok(failed(ConnectionTestStage::HeadObject, &error));
            }
        }

        match client
            .get_object()
            .bucket(&config.bucket)
            .key(&probe_key)
            .send()
            .await
        {
            Ok(get) => {
                let received = get
                    .body
                    .collect()
                    .await
                    .map_err(|_| {
                        BackupError::new(code::UPSTREAM_FAILURE, "读取探针对象响应失败".to_owned())
                    })?
                    .into_bytes();
                if received.as_ref() != body.as_slice() {
                    let _ = best_effort_delete(&client, config, &probe_key).await;
                    return Ok(ConnectionTestResult {
                        ok: false,
                        stage: ConnectionTestStage::GetObject.as_str(),
                        code: Some(code::UPSTREAM_FAILURE),
                        message: "探针对象内容不一致".to_owned(),
                    });
                }
            }
            Err(error) => {
                let _ = best_effort_delete(&client, config, &probe_key).await;
                return Ok(failed(ConnectionTestStage::GetObject, &error));
            }
        }

        let _ = best_effort_delete(&client, config, &probe_key).await;
        Ok(ConnectionTestResult {
            ok: true,
            stage: ConnectionTestStage::DeleteObject.as_str(),
            code: None,
            message: "连接测试成功".to_owned(),
        })
    }

    async fn upload_file(
        &self,
        config: &BackupStorageConfig,
        request: UploadObjectRequest,
    ) -> Result<(), BackupError> {
        let client = self.client(config);
        let file_size = tokio::fs::metadata(&request.source)
            .await
            .map_err(|_| BackupError::new(code::S3_UPLOAD_FAILED, "读取暂存归档失败".to_owned()))?
            .len();
        if file_size == 0 {
            return Err(BackupError::new(
                code::S3_UPLOAD_FAILED,
                "暂存归档为空，拒绝上传".to_owned(),
            ));
        }

        let created_at = request.metadata.created_at.to_rfc3339();
        let create = client
            .create_multipart_upload()
            .bucket(&config.bucket)
            .key(&request.object_key)
            .metadata("backup-id", &request.metadata.backup_id)
            .metadata("sha256", &request.metadata.sha256)
            .metadata("created-at", &created_at)
            .send()
            .await
            .map_err(map_s3_error)?;
        let upload_id = create
            .upload_id()
            .ok_or_else(|| {
                BackupError::new(
                    code::S3_UPLOAD_FAILED,
                    "S3 未返回 multipart upload id".to_owned(),
                )
            })?
            .to_owned();

        let upload = upload_parts(&client, config, &request, upload_id.clone(), file_size);
        let outcome = tokio::select! {
            result = upload => result,
            _ = request.cancellation.cancelled() => {
                Err(BackupError::new(code::CANCELLED, "上传已取消".to_owned()))
            }
        };
        if let Err(error) = &outcome {
            let _ = client
                .abort_multipart_upload()
                .bucket(&config.bucket)
                .key(&request.object_key)
                .upload_id(&upload_id)
                .send()
                .await;
            if error.code() != code::CANCELLED {
                let _ = client
                    .delete_object()
                    .bucket(&config.bucket)
                    .key(&request.object_key)
                    .send()
                    .await;
            }
        }
        outcome
    }

    async fn head_object(
        &self,
        config: &BackupStorageConfig,
        object_key: &str,
    ) -> Result<Option<BackupObjectMetadata>, BackupError> {
        let client = self.client(config);
        match client
            .head_object()
            .bucket(&config.bucket)
            .key(object_key)
            .send()
            .await
        {
            Ok(head) => {
                let size_bytes = head.content_length().filter(|&value| value >= 0);
                let metadata = head.metadata().cloned().unwrap_or_default();
                let sha256 = metadata.get("sha256").cloned().ok_or_else(|| {
                    BackupError::new(
                        code::REMOTE_VERIFICATION_FAILED,
                        "远端对象缺少 sha256 metadata".to_owned(),
                    )
                })?;
                let backup_id = metadata.get("backup-id").cloned().unwrap_or_default();
                let created_at = metadata
                    .get("created-at")
                    .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                    .map(|value| value.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now);
                let Some(size_bytes) = size_bytes else {
                    return Err(BackupError::new(
                        code::REMOTE_VERIFICATION_FAILED,
                        "远端对象缺少大小".to_owned(),
                    ));
                };
                Ok(Some(BackupObjectMetadata::new(
                    backup_id,
                    sha256,
                    created_at,
                    size_bytes as u64,
                )))
            }
            Err(error) if is_not_found(&error) => Ok(None),
            Err(error) => Err(map_s3_error(error)),
        }
    }

    async fn delete_object(
        &self,
        config: &BackupStorageConfig,
        object_key: &str,
    ) -> Result<(), BackupError> {
        let client = self.client(config);
        client
            .delete_object()
            .bucket(&config.bucket)
            .key(object_key)
            .send()
            .await
            .map(|_| ())
            .map_err(map_s3_error)
    }

    async fn presigned_download(
        &self,
        config: &BackupStorageConfig,
        object_key: &str,
        file_name: &str,
        ttl: Duration,
    ) -> Result<String, BackupError> {
        let client = self.client(config);
        let presigning = PresigningConfig::expires_in(ttl)
            .map_err(|_| BackupError::new(code::INVALID_CONFIG, "下载地址有效期非法".to_owned()))?;
        let request = client
            .get_object()
            .bucket(&config.bucket)
            .key(object_key)
            .response_content_disposition(format!("attachment; filename=\"{file_name}\""))
            .presigned(presigning)
            .await
            .map_err(map_s3_error)?;
        Ok(request.uri().to_owned())
    }
}

/// 并发分片上传 + CompleteMultipartUpload。
async fn upload_parts(
    client: &S3Client,
    config: &BackupStorageConfig,
    request: &UploadObjectRequest,
    upload_id: String,
    file_size: u64,
) -> Result<(), BackupError> {
    let total_parts = file_size.div_ceil(u64::try_from(PART_SIZE).unwrap_or(u64::MAX));
    let next_part = Arc::new(AtomicU64::new(1));
    let semaphore = Arc::new(Semaphore::new(CONCURRENCY));
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<(i32, String)>(CONCURRENCY * 2);

    let mut workers = Vec::with_capacity(CONCURRENCY);
    for _ in 0..CONCURRENCY {
        let client = client.clone();
        let semaphore = Arc::clone(&semaphore);
        let next_part = Arc::clone(&next_part);
        let sender = sender.clone();
        let source = request.source.clone();
        let bucket = config.bucket.clone();
        let key = request.object_key.clone();
        let upload_id = upload_id.clone();
        workers.push(tokio::spawn(async move {
            loop {
                let part = next_part.fetch_add(1, Ordering::Relaxed);
                if part > total_parts {
                    break;
                }
                let _permit = semaphore.acquire().await.map_err(|_| {
                    BackupError::new(code::S3_UPLOAD_FAILED, "上传并发闸门关闭".to_owned())
                })?;
                let offset = (part - 1) * (PART_SIZE as u64);
                let remaining = file_size.checked_sub(offset).ok_or_else(|| {
                    BackupError::new(code::S3_UPLOAD_FAILED, "分片偏移超出归档大小".to_owned())
                })?;
                let part_len = usize::try_from(remaining.min(PART_SIZE as u64)).map_err(|_| {
                    BackupError::new(code::S3_UPLOAD_FAILED, "分片大小溢出".to_owned())
                })?;
                let bytes = read_part(&source, offset, part_len).await?;
                let part_number = i32::try_from(part).map_err(|_| {
                    BackupError::new(code::S3_UPLOAD_FAILED, "分片号溢出".to_owned())
                })?;
                let response = client
                    .upload_part()
                    .bucket(&bucket)
                    .key(&key)
                    .upload_id(&upload_id)
                    .part_number(part_number)
                    .body(ByteStream::from(bytes))
                    .send()
                    .await
                    .map_err(map_s3_error)?;
                let etag = response
                    .e_tag()
                    .ok_or_else(|| {
                        BackupError::new(code::S3_UPLOAD_FAILED, "S3 未返回分片 ETag".to_owned())
                    })?
                    .to_owned();
                if sender.send((part_number, etag)).await.is_err() {
                    break;
                }
            }
            Ok::<(), BackupError>(())
        }));
    }
    drop(sender);

    let mut parts = Vec::new();
    while let Some((part, etag)) = receiver.recv().await {
        parts.push((part, etag));
    }
    for worker in workers {
        worker.await.map_err(|_| {
            BackupError::new(code::S3_UPLOAD_FAILED, "上传分片任务崩溃".to_owned())
        })??;
    }
    parts.sort_by_key(|(part, _)| *part);

    let completed = CompletedMultipartUpload::builder()
        .set_parts(Some(
            parts
                .into_iter()
                .map(|(part, etag)| {
                    CompletedPart::builder()
                        .part_number(part)
                        .e_tag(etag)
                        .build()
                })
                .collect(),
        ))
        .build();
    client
        .complete_multipart_upload()
        .bucket(&config.bucket)
        .key(&request.object_key)
        .upload_id(&upload_id)
        .multipart_upload(completed)
        .send()
        .await
        .map(|_| ())
        .map_err(map_s3_error)
}

/// 从文件中读取指定长度的完整分片。
async fn read_part(source: &Path, offset: u64, len: usize) -> Result<Vec<u8>, BackupError> {
    let mut file = tokio::fs::File::open(source)
        .await
        .map_err(|_| BackupError::new(code::S3_UPLOAD_FAILED, "打开暂存归档失败".to_owned()))?;
    file.seek(SeekFrom::Start(offset))
        .await
        .map_err(|_| BackupError::new(code::S3_UPLOAD_FAILED, "定位暂存归档失败".to_owned()))?;
    let mut buffer = vec![0_u8; len];
    // Tokio 默认单次文件读取最多 2 MiB；分片必须读取完整，不能把短读当作末尾分片上传。
    file.read_exact(&mut buffer)
        .await
        .map_err(|_| BackupError::new(code::S3_UPLOAD_FAILED, "读取暂存归档失败".to_owned()))?;
    Ok(buffer)
}

async fn best_effort_delete(
    client: &S3Client,
    config: &BackupStorageConfig,
    object_key: &str,
) -> Result<(), BackupError> {
    client
        .delete_object()
        .bucket(&config.bucket)
        .key(object_key)
        .send()
        .await
        .map(|_| ())
        .map_err(map_s3_error)
}

/// 探测失败结果。
fn failed(
    stage: ConnectionTestStage,
    error: &SdkError<impl std::fmt::Debug + ProvideErrorMetadata, HttpResponse>,
) -> ConnectionTestResult {
    let result = ConnectionTestResult {
        ok: false,
        stage: stage.as_str(),
        code: Some(s3_error_code(error)),
        message: s3_error_message(error),
    };
    tracing::warn!(
        stage = result.stage,
        code = result.code.unwrap_or_default(),
        message = %result.message,
        "S3 连接探针失败"
    );
    result
}

/// 稳定错误码。
fn s3_error_code<E: ProvideErrorMetadata>(error: &SdkError<E, HttpResponse>) -> &'static str {
    match status_of(error) {
        Some(401) => code::S3_AUTH_FAILED,
        Some(403) => code::S3_PERMISSION_DENIED,
        Some(404) => code::UPSTREAM_FAILURE,
        Some(500..=599) => code::UPSTREAM_FAILURE,
        _ => code::UPSTREAM_FAILURE,
    }
}

fn s3_error_message<E: ProvideErrorMetadata>(error: &SdkError<E, HttpResponse>) -> String {
    // 只取稳定的 S3 错误码（如 AccessDenied / NoSuchBucket / SignatureDoesNotMatch），
    // 不拼入原始 message，避免泄露请求细节。
    let detail = error.code().unwrap_or("unknown");
    match status_of(error) {
        Some(401) => format!("当前凭据认证失败（{detail}）"),
        Some(403) => format!("当前凭据没有目标前缀的读写权限（{detail}）"),
        Some(404) => format!("目标存储或对象不存在（{detail}）"),
        Some(500..=599) => format!("上游对象存储服务返回失败响应（{detail}）"),
        _ => format!("对象存储请求失败（{detail}）"),
    }
}

/// 提取 HTTP 状态码；非 ServiceError 返回 `None`。
fn status_of<E>(error: &SdkError<E, HttpResponse>) -> Option<u16> {
    match error {
        SdkError::ServiceError(service) => Some(service.raw().status().as_u16()),
        _ => None,
    }
}

/// 对象不存在的判定（HeadObject 的 404）。
fn is_not_found<E>(error: &SdkError<E, HttpResponse>) -> bool {
    status_of(error) == Some(404)
}

fn map_s3_error<E: ProvideErrorMetadata>(error: SdkError<E, HttpResponse>) -> BackupError {
    BackupError::new(s3_error_code(&error), s3_error_message(&error))
}
