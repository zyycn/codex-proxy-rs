//! 官方制品的有界下载与 ZIP 按需读取，使用 ETag 约束分段读取的一致性

use bytes::Bytes;
use gateway_core::lifecycle::CancellationToken;
use gateway_core::task::WorkerTaskError;
use reqwest::{Client, StatusCode, header};
use sha2::{Digest, Sha512};
use std::io::{self, Read, Seek, SeekFrom};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RANGE_BYTES: u64 = 16 * 1024 * 1024;
const ZIP_DIRECTORY_BYTES: u64 = 1024 * 1024;

pub(crate) struct ParseGuard(CancellationToken);
impl Drop for ParseGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub(crate) struct CancellableFile {
    file: std::fs::File,
    cancellation: CancellationToken,
}
impl CancellableFile {
    pub(crate) fn new(file: std::fs::File) -> (Self, ParseGuard) {
        let cancellation = CancellationToken::new();
        (
            Self {
                file,
                cancellation: cancellation.clone(),
            },
            ParseGuard(cancellation),
        )
    }
}
impl Read for CancellableFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancellation.is_cancelled() {
            return Err(io::Error::other("制品解析已取消"));
        }
        self.file.read(buffer)
    }
}

pub(crate) fn invalid() -> WorkerTaskError {
    WorkerTaskError::safe("Antigravity 发布包核验失败")
}

fn network(error: reqwest::Error) -> WorkerTaskError {
    invalid().with_source(error.without_url())
}

pub(crate) struct RemoteArtifact<'a> {
    client: &'a Client,
    url: String,
    size: u64,
    etag: header::HeaderValue,
}

impl<'a> RemoteArtifact<'a> {
    pub(crate) async fn open(client: &'a Client, url: String) -> Result<Self, WorkerTaskError> {
        let response = client.head(&url).send().await.map_err(network)?;
        if response.status() != StatusCode::OK {
            return Err(invalid());
        }
        // HEAD 没有响应体，reqwest 的 body 长度为零；制品长度来自 Content-Length
        let size: u64 = response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .ok_or_else(invalid)?;
        if size == 0 || size > MAX_ARTIFACT_BYTES {
            return Err(invalid());
        }
        let etag = response
            .headers()
            .get(header::ETAG)
            .cloned()
            .ok_or_else(invalid)?;
        if etag.as_bytes().starts_with(b"W/") {
            return Err(invalid());
        }
        Ok(Self {
            client,
            url,
            size,
            etag,
        })
    }

    pub(crate) async fn read(&self, start: u64, length: u64) -> Result<Bytes, WorkerTaskError> {
        let end = start
            .checked_add(length)
            .filter(|end| *end <= self.size)
            .ok_or_else(invalid)?;
        if length == 0 || length > MAX_RANGE_BYTES {
            return Err(invalid());
        }
        let mut response = self
            .client
            .get(&self.url)
            .header(header::ACCEPT_ENCODING, "identity")
            .header(header::IF_MATCH, self.etag.clone())
            .header(header::RANGE, format!("bytes={start}-{}", end - 1))
            .send()
            .await
            .map_err(network)?;
        if response.status() != StatusCode::PARTIAL_CONTENT
            || response
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                != Some(format!("bytes {start}-{}/{}", end - 1, self.size).as_str())
            || response.headers().get(header::ETAG) != Some(&self.etag)
        {
            return Err(invalid());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(network)? {
            if body.len() as u64 + chunk.len() as u64 > length {
                return Err(invalid());
            }
            body.extend_from_slice(&chunk);
        }
        if body.len() as u64 != length {
            return Err(invalid());
        }
        Ok(body.into())
    }

    pub(crate) async fn prefix(&self, maximum: u64) -> Result<Bytes, WorkerTaskError> {
        self.read(0, self.size.min(maximum)).await
    }
}

/// 只缓存目录和所需条目，未下载的区间不能被当作零填充数据读取
struct Segments {
    size: u64,
    position: u64,
    chunks: Vec<(u64, Bytes)>,
}
impl Read for Segments {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.position == self.size {
            return Ok(0);
        }
        for (start, bytes) in &self.chunks {
            if let Some(offset) = self.position.checked_sub(*start)
                && offset < bytes.len() as u64
            {
                let offset = offset as usize;
                let count = out.len().min(bytes.len() - offset);
                out[..count].copy_from_slice(&bytes[offset..offset + count]);
                self.position += count as u64;
                return Ok(count);
            }
        }
        Err(io::ErrorKind::UnexpectedEof.into())
    }
}
impl Seek for Segments {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::End(n) => i128::from(self.size) + i128::from(n),
            SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
        };
        self.position = u64::try_from(position).map_err(|_| io::ErrorKind::InvalidInput)?;
        Ok(self.position)
    }
}

pub(crate) struct RemoteZip<'a> {
    artifact: RemoteArtifact<'a>,
    tail: Bytes,
}
impl<'a> RemoteZip<'a> {
    pub(crate) async fn open(client: &'a Client, url: String) -> Result<Self, WorkerTaskError> {
        let artifact = RemoteArtifact::open(client, url).await?;
        let length = artifact.size.min(ZIP_DIRECTORY_BYTES);
        let tail = artifact.read(artifact.size - length, length).await?;
        validate_directory(&tail, artifact.size)?;
        Ok(Self { artifact, tail })
    }

    fn archive(
        &self,
        member: Option<(u64, Bytes)>,
    ) -> Result<zip::ZipArchive<Segments>, WorkerTaskError> {
        let mut chunks = vec![(
            self.artifact.size - self.tail.len() as u64,
            self.tail.clone(),
        )];
        chunks.extend(member);
        zip::ZipArchive::new(Segments {
            size: self.artifact.size,
            position: 0,
            chunks,
        })
        .map_err(|e| invalid().with_source(e))
    }

    pub(crate) async fn read(
        &self,
        name: &str,
        maximum: usize,
    ) -> Result<Vec<u8>, WorkerTaskError> {
        let archive = self.archive(None)?;
        let index = archive.index_for_name(name).ok_or_else(invalid)?;
        let entry = archive
            .by_index_data(index)
            .map_err(|e| invalid().with_source(e))?;
        if entry.size() > maximum as u64 || entry.is_dir() || entry.is_symlink() {
            return Err(invalid());
        }
        let start = entry.header_start();
        // local header 的两个变长字段都是 u16，读取预算覆盖它们而不信任压缩比
        let length = entry
            .compressed_size()
            .checked_add(30 + 2 * u64::from(u16::MAX))
            .ok_or_else(invalid)?
            .min(self.artifact.size.checked_sub(start).ok_or_else(invalid)?);
        let member = self.artifact.read(start, length).await?;
        let mut archive = self.archive(Some((start, member)))?;
        let file = archive
            .by_name(name)
            .map_err(|e| invalid().with_source(e))?;
        bounded(file, maximum)
    }
}

fn validate_directory(tail: &[u8], size: u64) -> Result<(), WorkerTaskError> {
    let offset = tail
        .windows(22)
        .rposition(|header| header.starts_with(b"PK\x05\x06"))
        .ok_or_else(invalid)?;
    let header = &tail[offset..offset + 22];
    let word = |offset| u16::from_le_bytes([header[offset], header[offset + 1]]);
    let dword = |offset| {
        u32::from_le_bytes([
            header[offset],
            header[offset + 1],
            header[offset + 2],
            header[offset + 3],
        ])
    };
    let directory_size = u64::from(dword(12));
    let directory_start = u64::from(dword(16));
    // 官方安装包小于 512 MiB，不接受分卷或 ZIP64，也不让目录计数触发无界分配
    if word(4) != 0
        || word(6) != 0
        || word(8) != word(10)
        || word(10) > 16_384
        || offset + 22 + usize::from(word(20)) != tail.len()
        || directory_start < size - tail.len() as u64
        || directory_size > ZIP_DIRECTORY_BYTES
        || directory_start + directory_size != size - tail.len() as u64 + offset as u64
    {
        return Err(invalid());
    }
    Ok(())
}

pub(crate) fn bounded(reader: impl Read, maximum: usize) -> Result<Vec<u8>, WorkerTaskError> {
    let mut body = Vec::new();
    reader
        .take(maximum as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|e| invalid().with_source(e))?;
    if body.len() > maximum {
        return Err(invalid());
    }
    Ok(body)
}

pub(crate) async fn download(
    client: &Client,
    url: &str,
    sha512: Option<&str>,
) -> Result<std::fs::File, WorkerTaskError> {
    let mut response = client
        .get(url)
        .header(header::ACCEPT_ENCODING, "identity")
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(network)?;
    if response.status() != StatusCode::OK
        || response
            .content_length()
            .is_some_and(|size| size > MAX_ARTIFACT_BYTES)
    {
        return Err(invalid());
    }
    let file = tempfile::tempfile().map_err(|e| invalid().with_source(e))?;
    let mut file = tokio::fs::File::from_std(file);
    let mut count = 0u64;
    let mut digest = Sha512::new();
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        count += chunk.len() as u64;
        if count > MAX_ARTIFACT_BYTES {
            return Err(invalid());
        }
        digest.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| invalid().with_source(e))?;
    }
    if let Some(expected) = sha512
        && hex::encode(digest.finalize()) != expected
    {
        return Err(invalid());
    }
    file.flush().await.map_err(|e| invalid().with_source(e))?;
    let mut file = file.into_std().await;
    file.rewind().map_err(|e| invalid().with_source(e))?;
    Ok(file)
}
