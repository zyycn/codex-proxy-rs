//! 将同一 ETag 的有界 HTTP Range 暴露为归档库需要的 Read + Seek。

use std::io::{self, Read, Seek, SeekFrom};
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use reqwest::{Client, StatusCode, header};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::super::desktop_artifact::parse_content_range;

const CHUNK: u64 = 1024 * 1024;
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

pub(super) fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "官方 Desktop 制品结构或版本不完整",
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ArtifactIdentity {
    pub size: u64,
    pub etag: String,
}

pub struct RemoteFile {
    client: Client,
    url: String,
    pub identity: ArtifactIdentity,
    runtime: tokio::runtime::Handle,
    cancel: CancellationToken,
    deadline: Instant,
    position: u64,
    buffer_start: u64,
    buffer: Vec<u8>,
    downloaded: u64,
}

impl RemoteFile {
    pub async fn open(client: Client, url: String, cancel: CancellationToken) -> io::Result<Self> {
        let (identity, _) = request(&client, &url, 0, 0, None).await?;
        Ok(Self {
            client,
            url,
            identity,
            cancel,
            runtime: tokio::runtime::Handle::current(),
            deadline: Instant::now() + Duration::from_secs(300),
            position: 0,
            buffer_start: 0,
            buffer: Vec::new(),
            downloaded: 0,
        })
    }
}

impl Read for RemoteFile {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() || Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Desktop 制品读取已取消或超时",
            ));
        }
        if output.is_empty() || self.position >= self.identity.size {
            return Ok(0);
        }
        if self.position < self.buffer_start
            || self.position >= self.buffer_start + self.buffer.len() as u64
        {
            let end = self
                .position
                .saturating_add(CHUNK - 1)
                .min(self.identity.size - 1);
            self.downloaded += end - self.position + 1;
            if self.downloaded > MAX_DOWNLOAD {
                return Err(invalid());
            }
            let (_, bytes) = self.runtime.block_on(async {
                tokio::select! {
                    () = self.cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "Desktop 制品读取已取消")),
                    result = request(&self.client, &self.url, self.position, end, Some(&self.identity)) => result,
                }
            })?;
            self.buffer_start = self.position;
            self.buffer = bytes;
        }
        let offset = (self.position - self.buffer_start) as usize;
        let count = output.len().min(self.buffer.len() - offset);
        output[..count].copy_from_slice(&self.buffer[offset..offset + count]);
        self.position += count as u64;
        Ok(count)
    }
}

pub(super) fn seek_position(position: u64, size: u64, seek: SeekFrom) -> io::Result<u64> {
    let next = match seek {
        SeekFrom::Start(offset) => i128::from(offset),
        SeekFrom::Current(offset) => i128::from(position) + i128::from(offset),
        SeekFrom::End(offset) => i128::from(size) + i128::from(offset),
    };
    u64::try_from(next)
        .ok()
        .filter(|next| *next <= size)
        .ok_or_else(invalid)
}

impl Seek for RemoteFile {
    fn seek(&mut self, seek: SeekFrom) -> io::Result<u64> {
        self.position = seek_position(self.position, self.identity.size, seek)?;
        Ok(self.position)
    }
}

async fn request(
    client: &Client,
    url: &str,
    start: u64,
    end: u64,
    expected: Option<&ArtifactIdentity>,
) -> io::Result<(ArtifactIdentity, Vec<u8>)> {
    let mut request = client
        .get(url)
        .header(header::RANGE, format!("bytes={start}-{end}"));
    if let Some(expected) = expected {
        request = request.header(header::IF_MATCH, &expected.etag);
    }
    let response = request
        .send()
        .await
        .map_err(|_| io::Error::other("官方 Desktop 制品请求失败"))?;
    let range = response
        .headers()
        .get(header::CONTENT_RANGE)
        .and_then(|h| h.to_str().ok())
        .and_then(parse_content_range)
        .ok_or_else(invalid)?;
    let etag = response
        .headers()
        .get(header::ETAG)
        .and_then(|h| h.to_str().ok())
        .filter(|s| s.starts_with('"') && s.ends_with('"') && s.len() <= 128)
        .ok_or_else(invalid)?
        .to_owned();
    let identity = ArtifactIdentity {
        size: range.2,
        etag,
    };
    if response.status() != StatusCode::PARTIAL_CONTENT
        || range.0 != start
        || range.1 != end
        || identity.size == 0
        || identity.size > 2 * 1024 * 1024 * 1024
        || expected.is_some_and(|expected| *expected != identity)
        || response.content_length() != Some(end - start + 1)
    {
        return Err(invalid());
    }
    let length = usize::try_from(end - start + 1).map_err(|_| invalid())?;
    let mut bytes = Vec::with_capacity(length);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| io::Error::other("官方 Desktop 制品读取失败"))?;
        if bytes.len().saturating_add(chunk.len()) > length {
            return Err(invalid());
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() != length {
        return Err(invalid());
    }
    Ok((identity, bytes))
}
