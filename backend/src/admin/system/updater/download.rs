//! 自更新下载、checksum 与 URL 校验。

use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::Duration,
};

use sha2::{Digest, Sha256};

use crate::admin::response::AdminError;

use super::{
    bad_gateway_with, bad_request_with, emit_update_event, format_bytes, internal_error_with,
    UpdateLogLevel,
};

#[derive(Debug, Clone, Copy)]
pub(super) struct DownloadProgress<'a> {
    pub operation_id: &'a str,
    pub total_size: u64,
}

pub(super) async fn download_file(
    url: &str,
    dest: &Path,
    max_size: u64,
    progress: Option<DownloadProgress<'_>>,
) -> Result<(), AdminError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(internal_error_with("Failed to create HTTP client"))?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(bad_gateway_with("Download failed"))?;
    if !response.status().is_success() {
        return Err(AdminError::bad_gateway(format!(
            "Download failed with {}",
            response.status()
        )));
    }
    let mut file =
        fs::File::create(dest).map_err(internal_error_with("Failed to create download file"))?;
    let mut downloaded = 0_u64;
    let mut next_progress = 10_u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(bad_gateway_with("Download stream failed"))?
    {
        downloaded += chunk.len() as u64;
        if downloaded > max_size {
            return Err(AdminError::bad_request("Download exceeds max allowed size"));
        }
        file.write_all(&chunk)
            .map_err(internal_error_with("Failed to write download"))?;
        if let Some(progress) = progress {
            let percent = download_percent(downloaded, progress.total_size);
            if progress.total_size > 0
                && (percent >= next_progress || downloaded >= progress.total_size)
            {
                emit_update_event(
                    UpdateLogLevel::Info,
                    Some(progress.operation_id),
                    Some("download"),
                    format!(
                        "已下载 {} / {} ({percent}%)",
                        format_bytes(downloaded),
                        format_bytes(progress.total_size)
                    ),
                );
                next_progress = percent.saturating_add(10);
            }
        }
    }
    Ok(())
}

pub(super) async fn verify_checksum(
    file_path: &Path,
    file_name: &str,
    checksum_url: &str,
) -> Result<(), AdminError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(internal_error_with("Failed to create HTTP client"))?;
    let body = client
        .get(checksum_url)
        .send()
        .await
        .map_err(bad_gateway_with("Checksum download failed"))?;
    let status = body.status();
    if !status.is_success() {
        return Err(AdminError::bad_gateway(format!(
            "Checksum download failed with {status}"
        )));
    }
    let body = body
        .text()
        .await
        .map_err(bad_gateway_with("Checksum read failed"))?;

    let expected = body.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?;
        (Path::new(name).file_name()?.to_string_lossy() == file_name).then(|| hash.to_string())
    });
    let expected = expected.ok_or_else(|| AdminError::bad_gateway("Checksum not found"))?;
    let actual = sha256_file(file_path)?;
    if expected.eq_ignore_ascii_case(&actual) {
        Ok(())
    } else {
        Err(AdminError::bad_gateway("Checksum mismatch"))
    }
}

pub(super) fn validate_download_url(
    raw_url: &str,
    github_api_base: &str,
) -> Result<(), AdminError> {
    let url = reqwest::Url::parse(raw_url).map_err(bad_request_with("Invalid download URL"))?;
    if local_update_test_download_allowed(&url, github_api_base) {
        return Ok(());
    }
    if url.scheme() != "https" {
        return Err(AdminError::bad_request(
            "Only HTTPS release downloads are allowed",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| AdminError::bad_request("Download URL is missing host"))?;
    let allowed = host == "github.com"
        || host.ends_with(".github.com")
        || host == "objects.githubusercontent.com"
        || host.ends_with(".objects.githubusercontent.com");
    if allowed {
        Ok(())
    } else {
        Err(AdminError::bad_request(format!(
            "Download host is not allowed: {host}"
        )))
    }
}

pub(super) fn validate_github_api_base(raw_url: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(raw_url)
        .map_err(|error| format!("Invalid GitHub API base: {error}"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("GitHub API base must not include credentials, query, or fragment".to_string());
    }
    if url.path().trim_end_matches('/') != "/repos" {
        return Err("GitHub API base path must be /repos".to_string());
    }
    if url_host_is_loopback(&url) {
        return Ok(());
    }
    if url.scheme() != "https" {
        return Err("GitHub API base must use HTTPS".to_string());
    }
    if url.host_str() != Some("api.github.com") {
        return Err("GitHub API base must be https://api.github.com/repos".to_string());
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, AdminError> {
    let mut file =
        fs::File::open(path).map_err(internal_error_with("Failed to open checksum file"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(internal_error_with("Failed to read checksum file"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn local_update_test_download_allowed(url: &reqwest::Url, github_api_base: &str) -> bool {
    if url.scheme() != "http" || !url_host_is_loopback(url) {
        return false;
    }
    let Ok(api_base) = reqwest::Url::parse(github_api_base) else {
        return false;
    };
    api_base.scheme() == "http"
        && url_host_is_loopback(&api_base)
        && url.host_str() == api_base.host_str()
        && url.port_or_known_default() == api_base.port_or_known_default()
}

fn url_host_is_loopback(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host == "localhost"
            || host == "127.0.0.1"
            || host == "::1"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

fn download_percent(downloaded: u64, total_size: u64) -> u64 {
    if total_size == 0 {
        return 0;
    }
    downloaded
        .saturating_mul(100)
        .saturating_div(total_size)
        .min(100)
}
