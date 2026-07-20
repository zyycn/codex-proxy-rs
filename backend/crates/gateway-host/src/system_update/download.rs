//! 受限制的 Release 下载、重定向信任链和 SHA-256 校验。

use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use futures::StreamExt as _;
use sha2::Digest as _;

use super::release::{download_client, validate_download_url};
use super::{OperationError, UpdateEvents, invalid, upstream};

pub(crate) const MAX_DOWNLOAD_SIZE: u64 = 500 * 1024 * 1024;
pub(crate) const MAX_CHECKSUM_SIZE: u64 = 1024 * 1024;

pub(crate) async fn download_file(
    url: &str,
    destination: &Path,
    expected_size: u64,
    api_base: &str,
    operation_id: &str,
    events: &UpdateEvents,
) -> Result<(), OperationError> {
    validate_download_url(url, api_base)?;
    if expected_size == 0 || expected_size > MAX_DOWNLOAD_SIZE {
        return Err(invalid("release archive size is invalid"));
    }
    let client = download_client(api_base, Duration::from_secs(120))?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .map_err(|error| upstream(format!("release download failed: {error}")))?;
    if !response.status().is_success() {
        return Err(upstream(format!(
            "release download failed with {}",
            response.status()
        )));
    }
    if response
        .content_length()
        .is_some_and(|size| size != expected_size)
    {
        return Err(upstream("downloaded size does not match release metadata"));
    }
    let mut file = fs::File::create(destination)
        .map_err(|error| super::internal(format!("failed to create download file: {error}")))?;
    let mut stream = response.bytes_stream();
    let mut downloaded = 0_u64;
    let mut last_percent = 0_u8;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| upstream(format!("download stream failed: {error}")))?;
        downloaded = downloaded.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if downloaded > expected_size {
            return Err(invalid("download exceeds declared release size"));
        }
        file.write_all(&chunk)
            .map_err(|error| super::internal(format!("failed to write download: {error}")))?;
        let percent = download_percent(downloaded, expected_size);
        if percent > last_percent {
            last_percent = percent;
            events.emit_progress(operation_id, "downloading release archive", percent);
        }
    }
    file.sync_all()
        .map_err(|error| super::internal(format!("failed to sync download: {error}")))?;
    if downloaded != expected_size {
        return Err(upstream("downloaded size does not match release metadata"));
    }
    Ok(())
}

pub(crate) async fn verify_checksum(
    file_path: &Path,
    file_name: &str,
    checksum_url: &str,
    checksum_size: u64,
    api_base: &str,
) -> Result<(), OperationError> {
    validate_download_url(checksum_url, api_base)?;
    if checksum_size == 0 || checksum_size > MAX_CHECKSUM_SIZE {
        return Err(invalid("release checksum size is invalid"));
    }
    let client = download_client(api_base, Duration::from_secs(30))?;
    let response = client
        .get(checksum_url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .map_err(|error| upstream(format!("checksum download failed: {error}")))?;
    if !response.status().is_success() {
        return Err(upstream(format!(
            "checksum download failed with {}",
            response.status()
        )));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| upstream(format!("checksum read failed: {error}")))?;
    if bytes.len() > MAX_CHECKSUM_SIZE as usize || bytes.len() as u64 != checksum_size {
        return Err(invalid("checksum document size is invalid"));
    }
    let body =
        std::str::from_utf8(&bytes).map_err(|_| invalid("checksum document is not UTF-8"))?;
    let expected = checksum_for(body, file_name)
        .ok_or_else(|| upstream("release checksum entry is missing"))?;
    let actual = sha256_file(file_path)?;
    if !expected.eq_ignore_ascii_case(&actual) {
        return Err(upstream("release checksum mismatch"));
    }
    Ok(())
}

fn checksum_for(body: &str, file_name: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let hash = fields.next()?;
        let name = fields.next()?.trim_start_matches('*');
        (hash.len() == 64
            && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            && Path::new(name).file_name()?.to_string_lossy() == file_name)
            .then(|| hash.to_owned())
    })
}

fn sha256_file(path: &Path) -> Result<String, OperationError> {
    let mut file = fs::File::open(path)
        .map_err(|error| super::internal(format!("failed to open checksum file: {error}")))?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| super::internal(format!("failed to read checksum file: {error}")))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn download_percent(downloaded: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    u8::try_from(
        downloaded
            .saturating_mul(100)
            .saturating_div(total)
            .min(100),
    )
    .unwrap_or(100)
}
