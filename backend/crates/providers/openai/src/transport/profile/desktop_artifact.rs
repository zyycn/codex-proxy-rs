//! 有界读取 Codex Desktop ZIP 内嵌的 arm64 Core 版本。

use std::cmp;

use flate2::{Decompress, FlushDecompress, Status};
use futures::StreamExt as _;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use reqwest::{Client, StatusCode};
use url::Url;

const OFFICIAL_ARTIFACT_HOST: &str = "persistent.oaistatic.com";
const EOCD_SIGNATURE: &[u8; 4] = b"PK\x05\x06";
const CENTRAL_SIGNATURE: &[u8; 4] = b"PK\x01\x02";
const LOCAL_SIGNATURE: &[u8; 4] = b"PK\x03\x04";
const CORE_PATH_SUFFIX: &[u8] = b"/Contents/Resources/codex";
const CORE_VERSION_ANCHOR: &[u8] = b"codex-mcp-client/";
const MAX_EOCD_SEARCH_BYTES: u64 = 65_535 + 22;
const MAX_CENTRAL_DIRECTORY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RANGE_BYTES: u64 = 16 * 1024 * 1024;
const CORE_RANGE_CHUNK_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CORE_COMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CORE_UNCOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;
const MAX_VERSION_BYTES: usize = 64;
const MACHO_HEADER_BYTES: usize = 8;
const MACHO_64_LE_MAGIC: [u8; 4] = [0xcf, 0xfa, 0xed, 0xfe];
const MACHO_ARM64_CPU_TYPE: u32 = 0x0100_000c;

#[derive(Debug, thiserror::Error)]
pub enum CodexDesktopArtifactError {
    #[error("Codex Desktop artifact URL is invalid")]
    InvalidUrl,
    #[error("Codex Desktop artifact URL is not an approved official ZIP")]
    UnapprovedUrl,
    #[error("Codex Desktop artifact size is invalid")]
    InvalidSize,
    #[error("Codex Desktop artifact range request failed")]
    Http(#[from] reqwest::Error),
    #[error("Codex Desktop artifact did not honor an exact range request")]
    RangeUnsupported,
    #[error("Codex Desktop artifact returned an invalid range")]
    InvalidRange,
    #[error("Codex Desktop artifact ZIP metadata is invalid or unsupported")]
    InvalidArchive,
    #[error("Codex Desktop artifact does not contain exactly one bundled Core")]
    MissingCore,
    #[error("Codex Desktop bundled Core format is unsupported")]
    UnsupportedCore,
    #[error("Codex Desktop bundled Core exceeds the scan limit")]
    CoreTooLarge,
    #[error("Codex Desktop bundled Core version marker is missing or invalid")]
    MissingCoreVersion,
}

#[derive(Debug)]
struct CentralDirectory {
    offset: u64,
    size: u64,
    entries: u16,
}

/// ZIP central directory 中已经校验的 Codex Core 条目。
#[derive(Debug)]
pub struct CoreEntry {
    /// ZIP 内完整条目名。
    pub name: Vec<u8>,
    flags: u16,
    compression: u16,
    /// 压缩后的字节数。
    pub compressed_size: u64,
    /// 解压后的字节数。
    pub uncompressed_size: u64,
    /// local file header 在制品内的偏移。
    pub local_header_offset: u64,
}

pub(super) async fn fetch_codex_core_version(
    client: &Client,
    artifact_url: &str,
    artifact_size: u64,
) -> Result<String, CodexDesktopArtifactError> {
    let url = validate_artifact(artifact_url, artifact_size)?;
    let tail_start = artifact_size.saturating_sub(MAX_EOCD_SEARCH_BYTES);
    let tail = read_range(client, &url, artifact_size, tail_start, artifact_size - 1).await?;
    let directory = parse_eocd(&tail, tail_start, artifact_size)?;
    let directory_end = directory
        .offset
        .checked_add(directory.size)
        .and_then(|end| end.checked_sub(1))
        .ok_or(CodexDesktopArtifactError::InvalidArchive)?;
    let central = if directory.offset >= tail_start && directory_end < artifact_size {
        let start = usize::try_from(directory.offset - tail_start)
            .map_err(|_| CodexDesktopArtifactError::InvalidArchive)?;
        let end = usize::try_from(directory_end - tail_start + 1)
            .map_err(|_| CodexDesktopArtifactError::InvalidArchive)?;
        tail.get(start..end)
            .ok_or(CodexDesktopArtifactError::InvalidArchive)?
            .to_vec()
    } else {
        read_range(client, &url, artifact_size, directory.offset, directory_end).await?
    };
    let entry = find_core_entry(&central, directory.entries)?;
    let data_offset = core_data_offset(client, &url, artifact_size, &entry).await?;
    scan_core_version(client, &url, artifact_size, data_offset, &entry).await
}

pub(super) fn validate_codex_artifact(
    artifact_url: &str,
    artifact_size: u64,
) -> Result<(), CodexDesktopArtifactError> {
    validate_artifact(artifact_url, artifact_size).map(drop)
}

fn validate_artifact(
    artifact_url: &str,
    artifact_size: u64,
) -> Result<Url, CodexDesktopArtifactError> {
    if artifact_size == 0 || artifact_size > u64::from(u32::MAX) {
        return Err(CodexDesktopArtifactError::InvalidSize);
    }
    let url = Url::parse(artifact_url).map_err(|_| CodexDesktopArtifactError::InvalidUrl)?;
    if url.scheme() != "https"
        || url.host_str() != Some(OFFICIAL_ARTIFACT_HOST)
        || !url.path().to_ascii_lowercase().ends_with(".zip")
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(CodexDesktopArtifactError::UnapprovedUrl);
    }
    Ok(url)
}

async fn read_range(
    client: &Client,
    url: &Url,
    artifact_size: u64,
    start: u64,
    end: u64,
) -> Result<Vec<u8>, CodexDesktopArtifactError> {
    let expected = end
        .checked_sub(start)
        .and_then(|length| length.checked_add(1))
        .filter(|length| *length <= MAX_RANGE_BYTES)
        .ok_or(CodexDesktopArtifactError::InvalidRange)?;
    let response = client
        .get(url.clone())
        .header(RANGE, format!("bytes={start}-{end}"))
        .send()
        .await?;
    if response.status() != StatusCode::PARTIAL_CONTENT {
        return Err(CodexDesktopArtifactError::RangeUnsupported);
    }
    let content_range = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .ok_or(CodexDesktopArtifactError::InvalidRange)?;
    if parse_content_range(content_range) != Some((start, end, artifact_size)) {
        return Err(CodexDesktopArtifactError::InvalidRange);
    }
    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        != Some(expected)
    {
        return Err(CodexDesktopArtifactError::InvalidRange);
    }

    let capacity =
        usize::try_from(expected).map_err(|_| CodexDesktopArtifactError::InvalidRange)?;
    let mut body = Vec::with_capacity(capacity);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body
            .len()
            .checked_add(chunk.len())
            .is_none_or(|length| length > capacity)
        {
            return Err(CodexDesktopArtifactError::InvalidRange);
        }
        body.extend_from_slice(&chunk);
    }
    if body.len() != capacity {
        return Err(CodexDesktopArtifactError::InvalidRange);
    }
    Ok(body)
}

/// 解析严格的 `bytes start-end/size` Content-Range。
#[must_use]
pub fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.strip_prefix("bytes ")?;
    let (range, size) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, size.parse().ok()?))
}

fn parse_eocd(
    tail: &[u8],
    tail_start: u64,
    artifact_size: u64,
) -> Result<CentralDirectory, CodexDesktopArtifactError> {
    let position = tail
        .windows(EOCD_SIGNATURE.len())
        .rposition(|window| window == EOCD_SIGNATURE)
        .ok_or(CodexDesktopArtifactError::InvalidArchive)?;
    let eocd = tail
        .get(position..)
        .filter(|value| value.len() >= 22)
        .ok_or(CodexDesktopArtifactError::InvalidArchive)?;
    let comment_length = usize::from(le_u16(eocd, 20)?);
    if eocd.len() != 22 + comment_length || le_u16(eocd, 4)? != 0 || le_u16(eocd, 6)? != 0 {
        return Err(CodexDesktopArtifactError::InvalidArchive);
    }
    let entries_on_disk = le_u16(eocd, 8)?;
    let entries = le_u16(eocd, 10)?;
    let size = u64::from(le_u32(eocd, 12)?);
    let offset = u64::from(le_u32(eocd, 16)?);
    if entries == 0
        || entries != entries_on_disk
        || entries == u16::MAX
        || size == u64::from(u32::MAX)
        || offset == u64::from(u32::MAX)
        || size == 0
        || size > MAX_CENTRAL_DIRECTORY_BYTES
    {
        return Err(CodexDesktopArtifactError::InvalidArchive);
    }
    let eocd_offset = tail_start
        .checked_add(
            u64::try_from(position).map_err(|_| CodexDesktopArtifactError::InvalidArchive)?,
        )
        .ok_or(CodexDesktopArtifactError::InvalidArchive)?;
    if offset
        .checked_add(size)
        .is_none_or(|end| end > eocd_offset || end > artifact_size)
    {
        return Err(CodexDesktopArtifactError::InvalidArchive);
    }
    Ok(CentralDirectory {
        offset,
        size,
        entries,
    })
}

/// 在 central directory 中定位唯一的 bundled Codex Core。
///
/// # Errors
///
/// ZIP 元数据无效、条目不受支持，或 Core 不唯一时返回制品错误。
pub fn find_core_entry(
    central: &[u8],
    expected_entries: u16,
) -> Result<CoreEntry, CodexDesktopArtifactError> {
    let mut cursor = 0_usize;
    let mut core = None;
    for _ in 0..expected_entries {
        let header = central
            .get(cursor..cursor.saturating_add(46))
            .filter(|header| header.get(..4) == Some(CENTRAL_SIGNATURE.as_slice()))
            .ok_or(CodexDesktopArtifactError::InvalidArchive)?;
        let name_length = usize::from(le_u16(header, 28)?);
        let extra_length = usize::from(le_u16(header, 30)?);
        let comment_length = usize::from(le_u16(header, 32)?);
        let entry_length = 46_usize
            .checked_add(name_length)
            .and_then(|length| length.checked_add(extra_length))
            .and_then(|length| length.checked_add(comment_length))
            .ok_or(CodexDesktopArtifactError::InvalidArchive)?;
        let entry = central
            .get(cursor..cursor.saturating_add(entry_length))
            .ok_or(CodexDesktopArtifactError::InvalidArchive)?;
        let name = entry
            .get(46..46 + name_length)
            .ok_or(CodexDesktopArtifactError::InvalidArchive)?;
        if name.ends_with(CORE_PATH_SUFFIX) {
            if core.is_some() {
                return Err(CodexDesktopArtifactError::MissingCore);
            }
            let flags = le_u16(header, 8)?;
            let compression = le_u16(header, 10)?;
            let compressed_size = u64::from(le_u32(header, 20)?);
            let uncompressed_size = u64::from(le_u32(header, 24)?);
            let local_header_offset = u64::from(le_u32(header, 42)?);
            if flags & 1 != 0
                || compression != 8
                || compressed_size == 0
                || compressed_size == u64::from(u32::MAX)
                || compressed_size > MAX_CORE_COMPRESSED_BYTES
                || uncompressed_size == 0
                || uncompressed_size == u64::from(u32::MAX)
                || uncompressed_size > MAX_CORE_UNCOMPRESSED_BYTES
                || local_header_offset == u64::from(u32::MAX)
            {
                return Err(CodexDesktopArtifactError::UnsupportedCore);
            }
            core = Some(CoreEntry {
                name: name.to_vec(),
                flags,
                compression,
                compressed_size,
                uncompressed_size,
                local_header_offset,
            });
        }
        cursor = cursor
            .checked_add(entry_length)
            .ok_or(CodexDesktopArtifactError::InvalidArchive)?;
    }
    core.ok_or(CodexDesktopArtifactError::MissingCore)
}

async fn core_data_offset(
    client: &Client,
    url: &Url,
    artifact_size: u64,
    entry: &CoreEntry,
) -> Result<u64, CodexDesktopArtifactError> {
    let fixed_end = entry
        .local_header_offset
        .checked_add(29)
        .filter(|end| *end < artifact_size)
        .ok_or(CodexDesktopArtifactError::InvalidArchive)?;
    let fixed = read_range(
        client,
        url,
        artifact_size,
        entry.local_header_offset,
        fixed_end,
    )
    .await?;
    if fixed.get(..4) != Some(LOCAL_SIGNATURE.as_slice())
        || le_u16(&fixed, 6)? != entry.flags
        || le_u16(&fixed, 8)? != entry.compression
    {
        return Err(CodexDesktopArtifactError::InvalidArchive);
    }
    let name_length = u64::from(le_u16(&fixed, 26)?);
    let extra_length = u64::from(le_u16(&fixed, 28)?);
    let variable_length = name_length
        .checked_add(extra_length)
        .ok_or(CodexDesktopArtifactError::InvalidArchive)?;
    let variable_start = entry
        .local_header_offset
        .checked_add(30)
        .ok_or(CodexDesktopArtifactError::InvalidArchive)?;
    let data_offset = variable_start
        .checked_add(variable_length)
        .ok_or(CodexDesktopArtifactError::InvalidArchive)?;
    let data_end = data_offset
        .checked_add(entry.compressed_size)
        .ok_or(CodexDesktopArtifactError::InvalidArchive)?;
    if data_end > artifact_size || name_length == 0 {
        return Err(CodexDesktopArtifactError::InvalidArchive);
    }
    let variable_end = data_offset
        .checked_sub(1)
        .ok_or(CodexDesktopArtifactError::InvalidArchive)?;
    let variable = read_range(client, url, artifact_size, variable_start, variable_end).await?;
    let local_name_length =
        usize::try_from(name_length).map_err(|_| CodexDesktopArtifactError::InvalidArchive)?;
    if variable.get(..local_name_length) != Some(entry.name.as_slice()) {
        return Err(CodexDesktopArtifactError::InvalidArchive);
    }
    Ok(data_offset)
}

async fn scan_core_version(
    client: &Client,
    url: &Url,
    artifact_size: u64,
    data_offset: u64,
    entry: &CoreEntry,
) -> Result<String, CodexDesktopArtifactError> {
    let mut decompressor = Decompress::new(false);
    let mut scanner = CoreVersionScanner::default();
    let mut macho_header = Vec::with_capacity(MACHO_HEADER_BYTES);
    let mut compressed_read = 0_u64;
    let mut uncompressed_read = 0_u64;
    while compressed_read < entry.compressed_size {
        let chunk_length = cmp::min(
            CORE_RANGE_CHUNK_BYTES,
            entry.compressed_size - compressed_read,
        );
        let start = data_offset
            .checked_add(compressed_read)
            .ok_or(CodexDesktopArtifactError::InvalidArchive)?;
        let end = start
            .checked_add(chunk_length - 1)
            .ok_or(CodexDesktopArtifactError::InvalidArchive)?;
        let compressed = read_range(client, url, artifact_size, start, end).await?;
        compressed_read = compressed_read
            .checked_add(chunk_length)
            .ok_or(CodexDesktopArtifactError::CoreTooLarge)?;
        let final_chunk = compressed_read == entry.compressed_size;
        let mut input_offset = 0_usize;
        loop {
            if input_offset == compressed.len() && !final_chunk {
                break;
            }
            let mut output = [0_u8; 64 * 1024];
            let input_before = decompressor.total_in();
            let output_before = decompressor.total_out();
            let status = decompressor
                .decompress(
                    &compressed[input_offset..],
                    &mut output,
                    if final_chunk {
                        FlushDecompress::Finish
                    } else {
                        FlushDecompress::None
                    },
                )
                .map_err(|_| CodexDesktopArtifactError::UnsupportedCore)?;
            let consumed = usize::try_from(decompressor.total_in() - input_before)
                .map_err(|_| CodexDesktopArtifactError::CoreTooLarge)?;
            let produced = usize::try_from(decompressor.total_out() - output_before)
                .map_err(|_| CodexDesktopArtifactError::CoreTooLarge)?;
            if consumed == 0 && produced == 0 {
                return Err(CodexDesktopArtifactError::UnsupportedCore);
            }
            input_offset = input_offset
                .checked_add(consumed)
                .ok_or(CodexDesktopArtifactError::CoreTooLarge)?;
            uncompressed_read = uncompressed_read
                .checked_add(
                    u64::try_from(produced).map_err(|_| CodexDesktopArtifactError::CoreTooLarge)?,
                )
                .filter(|read| {
                    *read <= entry.uncompressed_size && *read <= MAX_CORE_UNCOMPRESSED_BYTES
                })
                .ok_or(CodexDesktopArtifactError::CoreTooLarge)?;
            let output = &output[..produced];
            if macho_header.len() < MACHO_HEADER_BYTES {
                let needed = MACHO_HEADER_BYTES - macho_header.len();
                macho_header.extend_from_slice(&output[..cmp::min(needed, output.len())]);
                if macho_header.len() == MACHO_HEADER_BYTES {
                    validate_macho_header(&macho_header)?;
                }
            }
            if let Some(version) = scanner.push(output)? {
                if macho_header.len() != MACHO_HEADER_BYTES {
                    return Err(CodexDesktopArtifactError::UnsupportedCore);
                }
                return Ok(version);
            }
            if status == Status::StreamEnd {
                if !final_chunk
                    || input_offset != compressed.len()
                    || uncompressed_read != entry.uncompressed_size
                    || macho_header.len() != MACHO_HEADER_BYTES
                {
                    return Err(CodexDesktopArtifactError::UnsupportedCore);
                }
                return scanner
                    .finish()?
                    .ok_or(CodexDesktopArtifactError::MissingCoreVersion);
            }
        }
    }
    Err(CodexDesktopArtifactError::UnsupportedCore)
}

fn validate_macho_header(header: &[u8]) -> Result<(), CodexDesktopArtifactError> {
    if header.get(..4) != Some(MACHO_64_LE_MAGIC.as_slice())
        || header
            .get(4..8)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u32::from_le_bytes)
            != Some(MACHO_ARM64_CPU_TYPE)
    {
        return Err(CodexDesktopArtifactError::UnsupportedCore);
    }
    Ok(())
}

/// 跨分块查找 bundled Core 版本标记的有界扫描器。
#[derive(Default)]
pub struct CoreVersionScanner {
    carry: Vec<u8>,
}

impl CoreVersionScanner {
    /// 推入一个解压分块，并在发现完整合法版本时返回它。
    ///
    /// # Errors
    ///
    /// 标记后的版本超过限制或不满足受支持格式时返回制品错误。
    pub fn push(&mut self, bytes: &[u8]) -> Result<Option<String>, CodexDesktopArtifactError> {
        let mut window = Vec::with_capacity(self.carry.len() + bytes.len());
        window.extend_from_slice(&self.carry);
        window.extend_from_slice(bytes);
        let mut search_start = 0_usize;
        while let Some(relative_position) = find_bytes(&window[search_start..], CORE_VERSION_ANCHOR)
        {
            let position = search_start + relative_position;
            if let Some(version) =
                parse_codex_semver(&window[position + CORE_VERSION_ANCHOR.len()..], false)?
            {
                return Ok(Some(version));
            }
            search_start = position + CORE_VERSION_ANCHOR.len();
        }
        let carry_length = cmp::min(window.len(), CORE_VERSION_ANCHOR.len() + MAX_VERSION_BYTES);
        self.carry.clear();
        self.carry
            .extend_from_slice(&window[window.len() - carry_length..]);
        Ok(None)
    }

    fn finish(&self) -> Result<Option<String>, CodexDesktopArtifactError> {
        let mut search_start = 0_usize;
        while let Some(relative_position) =
            find_bytes(&self.carry[search_start..], CORE_VERSION_ANCHOR)
        {
            let position = search_start + relative_position;
            if let Some(version) =
                parse_codex_semver(&self.carry[position + CORE_VERSION_ANCHOR.len()..], true)?
            {
                return Ok(Some(version));
            }
            search_start = position + CORE_VERSION_ANCHOR.len();
        }
        Ok(None)
    }
}

fn parse_codex_semver(
    bytes: &[u8],
    allow_end: bool,
) -> Result<Option<String>, CodexDesktopArtifactError> {
    let mut cursor = 0_usize;
    for component in 0..3 {
        let start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor == start || (cursor - start > 1 && bytes[start] == b'0') {
            return Ok(None);
        }
        if component < 2 {
            if bytes.get(cursor) != Some(&b'.') {
                return Ok(None);
            }
            cursor += 1;
        }
    }
    if bytes.get(cursor) == Some(&b'-') {
        cursor += 1;
        let channel = [b"alpha".as_slice(), b"beta".as_slice(), b"rc".as_slice()]
            .into_iter()
            .find(|channel| bytes.get(cursor..cursor + channel.len()) == Some(*channel));
        let Some(channel) = channel else {
            return Ok(None);
        };
        cursor += channel.len();
        let mut numeric_parts = 0_usize;
        while bytes.get(cursor) == Some(&b'.') {
            cursor += 1;
            let start = cursor;
            while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
                cursor += 1;
            }
            if cursor == start || (cursor - start > 1 && bytes[start] == b'0') {
                return Ok(None);
            }
            numeric_parts += 1;
        }
        if numeric_parts == 0 {
            return Ok(None);
        }
    }
    if cursor == bytes.len() && !allow_end {
        return Ok(None);
    }
    if bytes.get(cursor).is_some_and(u8::is_ascii_digit)
        || bytes.get(cursor) == Some(&b'.')
        || bytes
            .get(cursor)
            .is_some_and(|byte| matches!(*byte, b'+' | b'-'))
        || cursor > MAX_VERSION_BYTES
    {
        return Ok(None);
    }
    let version = std::str::from_utf8(&bytes[..cursor])
        .map_err(|_| CodexDesktopArtifactError::MissingCoreVersion)?;
    semver::Version::parse(version).map_err(|_| CodexDesktopArtifactError::MissingCoreVersion)?;
    Ok(Some(version.to_owned()))
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn le_u16(bytes: &[u8], offset: usize) -> Result<u16, CodexDesktopArtifactError> {
    bytes
        .get(offset..offset + 2)
        .and_then(|value| value.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or(CodexDesktopArtifactError::InvalidArchive)
}

fn le_u32(bytes: &[u8], offset: usize) -> Result<u32, CodexDesktopArtifactError> {
    bytes
        .get(offset..offset + 4)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(CodexDesktopArtifactError::InvalidArchive)
}
