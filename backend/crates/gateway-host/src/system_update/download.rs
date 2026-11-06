//! 受限制的 Release 下载、重定向信任链和 SHA-256 校验

use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use futures::StreamExt as _;
use sha2::Digest as _;

use super::events::UpdateEvents;
use super::release::{download_client, validate_download_url};
use super::{OperationError, invalid, upstream};

pub(crate) const MAX_DOWNLOAD_SIZE: u64 = 500 * 1024 * 1024;
const MAX_CHECKSUM_SIZE: u64 = 1024 * 1024;

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
        return Err(invalid("更新包大小无效或超出限制"));
    }
    let client = download_client(api_base, Duration::from_secs(120))?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .map_err(|error| super::error::http("更新包下载失败", error))?;
    if !response.status().is_success() {
        return Err(upstream(format!(
            "更新包下载失败（HTTP {}）",
            response.status().as_u16()
        )));
    }
    if response
        .content_length()
        .is_some_and(|size| size != expected_size)
    {
        return Err(upstream("下载文件大小与发行信息不一致"));
    }
    let mut file = fs::File::create(destination)
        .map_err(|error| super::internal("无法创建下载文件").with_source(error))?;
    let mut stream = response.bytes_stream();
    let mut downloaded = 0_u64;
    let mut next_progress = 10_u8;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| super::error::http("更新包下载中断", error))?;
        downloaded = downloaded.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if downloaded > expected_size {
            return Err(invalid("下载文件超过发行信息声明的大小，已停止更新"));
        }
        file.write_all(&chunk)
            .map_err(|error| super::internal("无法保存下载内容").with_source(error))?;
        let percent = download_percent(downloaded, expected_size);
        if percent >= next_progress || downloaded == expected_size {
            events.emit_progress(
                operation_id,
                &format!(
                    "已下载 {} / {} ({percent}%)",
                    format_bytes(downloaded),
                    format_bytes(expected_size)
                ),
                percent,
            );
            next_progress = percent.saturating_add(10);
        }
    }
    file.sync_all()
        .map_err(|error| super::internal("无法完成下载文件写入").with_source(error))?;
    if downloaded != expected_size {
        return Err(upstream("下载文件大小与发行信息不一致"));
    }
    Ok(())
}

pub(crate) async fn fetch_expected_checksum(
    file_name: &str,
    checksum_url: &str,
    checksum_size: u64,
    api_base: &str,
) -> Result<String, OperationError> {
    validate_download_url(checksum_url, api_base)?;
    if checksum_size == 0 || checksum_size > MAX_CHECKSUM_SIZE {
        return Err(invalid("校验文件大小无效或超出限制"));
    }
    let client = download_client(api_base, Duration::from_secs(30))?;
    let mut response = client
        .get(checksum_url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .map_err(|error| super::error::http("校验文件下载失败", error))?;
    if !response.status().is_success() {
        return Err(upstream(format!(
            "校验文件下载失败（HTTP {}）",
            response.status().as_u16()
        )));
    }
    let size = checksum_size as usize;
    let mut bytes = Vec::with_capacity(size);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| super::error::http("校验文件读取失败", error))?
    {
        if chunk.len() > size - bytes.len() {
            return Err(invalid("校验文件大小与发行信息不一致"));
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() != size {
        return Err(invalid("校验文件大小与发行信息不一致"));
    }
    let body =
        std::str::from_utf8(&bytes).map_err(|_| invalid("校验文件编码无效，无法校验更新包"))?;
    checksum_for(body, file_name).ok_or_else(|| upstream("校验文件中缺少当前更新包的校验值"))
}

pub(crate) fn verify_checksum(file_path: &Path, expected: &str) -> Result<(), OperationError> {
    let actual = sha256_file(file_path)?;
    if !expected.eq_ignore_ascii_case(&actual) {
        return Err(upstream("更新包校验失败，请重新下载"));
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
        .map_err(|error| super::internal("无法打开待校验文件").with_source(error))?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| super::internal("无法读取待校验文件").with_source(error))?;
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

pub(crate) fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    if bytes >= MIB {
        return format!("{:.1} MiB", bytes as f64 / MIB as f64);
    }
    if bytes >= KIB {
        return format!("{:.1} KiB", bytes as f64 / KIB as f64);
    }
    format!("{bytes} B")
}
