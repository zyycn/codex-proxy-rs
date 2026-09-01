//! RG-Adguard Store 页面解析与官方 Codex Desktop 下载回退。

use std::{collections::BTreeMap, time::Duration};

use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, TimeDelta, Utc};
use futures::StreamExt as _;
use gateway_admin::{
    model::client_distribution::{
        ClientArchitecture, ClientDownloadPackage, ClientDownloadSource,
        CodexDesktopWindowsDownloads,
    },
    ports::client_distribution::ClientDistributionResolver,
};
use reqwest::{Client, Url, redirect::Policy};
use scraper::{Html, Selector};
use tokio::sync::Mutex;

const RG_ADGUARD_ENDPOINT: &str = "https://store.rg-adguard.net/api/GetFiles";
const RG_ADGUARD_ORIGIN: &str = "https://store.rg-adguard.net";
const CODEX_STORE_PRODUCT_ID: &str = "9PLM9XGG6VKS";
const PACKAGE_PUBLISHER_ID: &str = "2p2nqsd0c76g0";
const MAXIMUM_RESPONSE_BYTES: usize = 512 * 1024;
const DYNAMIC_CACHE_SECONDS: i64 = 300;
const FALLBACK_CACHE_SECONDS: u64 = 60;
const MINIMUM_REMAINING_LINK_SECONDS: i64 = 600;

const OFFICIAL_X64_URL: &str = "https://persistent.oaistatic.com/codex-app-prod/ChatGPT-x64.msix";
const OFFICIAL_ARM64_URL: &str =
    "https://persistent.oaistatic.com/codex-app-prod/ChatGPT-arm64.msix";

/// 下载页请求或解析失败的稳定分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientDistributionResolutionErrorKind {
    /// 上游请求不可用。
    Unavailable,
    /// 上游响应不满足受信合同。
    InvalidResponse,
}

pub(crate) struct RgAdguardClientDistribution {
    client: Option<Client>,
    cache: Mutex<Option<CachedDownloads>>,
}

struct CachedDownloads {
    cached_at: std::time::Instant,
    valid_until: std::time::Instant,
    value: CodexDesktopWindowsDownloads,
}

impl RgAdguardClientDistribution {
    pub(crate) fn new() -> Self {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(12))
            .redirect(Policy::none())
            .build()
            .inspect_err(|_| {
                tracing::warn!(
                    "download resolver HTTP client initialization failed; official downloads will be used"
                );
            })
            .ok();
        Self {
            client,
            cache: Mutex::new(None),
        }
    }

    async fn resolve_uncached(&self) -> (CodexDesktopWindowsDownloads, Duration) {
        let now = Utc::now();
        match self.fetch_store_packages(now).await {
            Ok(packages) => {
                let (packages, warning) = complete_with_official_fallback(packages);
                let valid_seconds = packages
                    .iter()
                    .filter_map(|package| package.expires_at)
                    .map(|expires_at| {
                        (expires_at - now).num_seconds() - MINIMUM_REMAINING_LINK_SECONDS
                    })
                    .min()
                    .unwrap_or(DYNAMIC_CACHE_SECONDS)
                    .clamp(1, DYNAMIC_CACHE_SECONDS);
                (
                    CodexDesktopWindowsDownloads {
                        resolved_at: now,
                        cached: false,
                        warning,
                        packages,
                    },
                    Duration::from_secs(u64::try_from(valid_seconds).unwrap_or(1)),
                )
            }
            Err(error_kind) => {
                tracing::warn!(
                    error_kind = ?error_kind,
                    "RG-Adguard package resolution failed; using official downloads"
                );
                (
                    official_fallback(now),
                    Duration::from_secs(FALLBACK_CACHE_SECONDS),
                )
            }
        }
    }

    async fn fetch_store_packages(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<ClientDownloadPackage>, ClientDistributionResolutionErrorKind> {
        let client = self
            .client
            .as_ref()
            .ok_or(ClientDistributionResolutionErrorKind::Unavailable)?;
        let response = client
            .post(RG_ADGUARD_ENDPOINT)
            .header("origin", RG_ADGUARD_ORIGIN)
            .header("referer", format!("{RG_ADGUARD_ORIGIN}/"))
            .header(
                "user-agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/140 Safari/537.36",
            )
            .form(&[
                ("type", "ProductId"),
                ("url", CODEX_STORE_PRODUCT_ID),
                ("ring", "Retail"),
                ("lang", ""),
            ])
            .send()
            .await
            .map_err(|_| ClientDistributionResolutionErrorKind::Unavailable)?;
        if !response.status().is_success() {
            return Err(ClientDistributionResolutionErrorKind::Unavailable);
        }

        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ClientDistributionResolutionErrorKind::Unavailable)?;
            if bytes.len().saturating_add(chunk.len()) > MAXIMUM_RESPONSE_BYTES {
                return Err(ClientDistributionResolutionErrorKind::InvalidResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        let html = std::str::from_utf8(&bytes)
            .map_err(|_| ClientDistributionResolutionErrorKind::InvalidResponse)?;
        parse_store_packages(html, now)
    }
}

#[async_trait]
impl ClientDistributionResolver for RgAdguardClientDistribution {
    async fn resolve_codex_desktop_windows(&self, refresh: bool) -> CodexDesktopWindowsDownloads {
        let requested_at = std::time::Instant::now();
        let mut cache = self.cache.lock().await;
        if let Some(cached) = cache.as_ref()
            && cached.valid_until > std::time::Instant::now()
            && (!refresh || cached.cached_at >= requested_at)
        {
            let mut value = cached.value.clone();
            value.cached = true;
            return value;
        }

        let (value, ttl) = self.resolve_uncached().await;
        let cached_at = std::time::Instant::now();
        *cache = Some(CachedDownloads {
            cached_at,
            valid_until: cached_at + ttl,
            value: value.clone(),
        });
        value
    }
}

/// 从 RG-Adguard HTML 中选择每个受支持架构的最新有效主包。
///
/// # Errors
///
/// 页面没有任何满足包身份、下载地址和过期时间约束的结果时返回无效响应。
pub fn parse_store_packages(
    html: &str,
    now: DateTime<Utc>,
) -> Result<Vec<ClientDownloadPackage>, ClientDistributionResolutionErrorKind> {
    let document = Html::parse_document(html);
    let row_selector = Selector::parse("table.tftable tr")
        .map_err(|_| ClientDistributionResolutionErrorKind::InvalidResponse)?;
    let link_selector =
        Selector::parse("a").map_err(|_| ClientDistributionResolutionErrorKind::InvalidResponse)?;
    let cell_selector = Selector::parse("td")
        .map_err(|_| ClientDistributionResolutionErrorKind::InvalidResponse)?;
    let mut selected = BTreeMap::<ClientArchitecture, ParsedStorePackage>::new();

    for row in document.select(&row_selector) {
        let Some(link) = row.select(&link_selector).next() else {
            continue;
        };
        let file_name = link.text().collect::<String>().trim().to_owned();
        let Some((architecture, version)) = parse_package_file_name(&file_name) else {
            continue;
        };
        let Some(href) = link.value().attr("href") else {
            continue;
        };
        let Some(download_url) = validate_download_url(href) else {
            continue;
        };
        let cells = row
            .select(&cell_selector)
            .map(|cell| cell.text().collect::<String>().trim().to_owned())
            .collect::<Vec<_>>();
        let Some(expires_at) = cells.iter().find_map(|cell| parse_expiry(cell)) else {
            continue;
        };
        if expires_at <= now + TimeDelta::seconds(MINIMUM_REMAINING_LINK_SECONDS) {
            continue;
        }
        let size_bytes = cells.iter().find_map(|cell| parse_size_bytes(cell));
        let candidate = ParsedStorePackage {
            version,
            package: ClientDownloadPackage {
                architecture,
                source: ClientDownloadSource::MicrosoftStore,
                version: Some(version.to_string()),
                file_name,
                size_bytes,
                download_url,
                expires_at: Some(expires_at),
            },
        };
        let replace = selected
            .get(&architecture)
            .is_none_or(|existing| candidate.version > existing.version);
        if replace {
            selected.insert(architecture, candidate);
        }
    }

    if selected.is_empty() {
        return Err(ClientDistributionResolutionErrorKind::InvalidResponse);
    }
    Ok(selected.into_values().map(|value| value.package).collect())
}

#[derive(Debug)]
struct ParsedStorePackage {
    version: StorePackageVersion,
    package: ClientDownloadPackage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct StorePackageVersion([u64; 4]);

impl std::fmt::Display for StorePackageVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}.{}.{}.{}",
            self.0[0], self.0[1], self.0[2], self.0[3]
        )
    }
}

fn parse_package_file_name(file_name: &str) -> Option<(ClientArchitecture, StorePackageVersion)> {
    let suffix = format!("__{PACKAGE_PUBLISHER_ID}.msix");
    let body = file_name
        .strip_prefix("OpenAI.Codex_")?
        .strip_suffix(&suffix)?;
    let (version, architecture) = body.rsplit_once('_')?;
    let architecture = match architecture {
        "x64" => ClientArchitecture::X64,
        "arm64" => ClientArchitecture::Arm64,
        _ => return None,
    };
    let mut parts = version.split('.');
    let values = [
        parts.next()?.parse::<u64>().ok()?,
        parts.next()?.parse::<u64>().ok()?,
        parts.next()?.parse::<u64>().ok()?,
        parts.next()?.parse::<u64>().ok()?,
    ];
    if parts.next().is_some() {
        return None;
    }
    Some((architecture, StorePackageVersion(values)))
}

fn validate_download_url(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || !matches!(
            url.host_str(),
            Some("dl.delivery.mp.microsoft.com" | "tlu.dl.delivery.mp.microsoft.com")
        )
        || !url.path().starts_with("/filestreamingservice/files/")
        || url.path() == "/filestreamingservice/files/"
    {
        return None;
    }
    Some(url.into())
}

fn parse_expiry(value: &str) -> Option<DateTime<Utc>> {
    let value = value.strip_suffix(" GMT")?;
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|value| value.and_utc())
}

fn parse_size_bytes(value: &str) -> Option<u64> {
    let mut parts = value.split_ascii_whitespace();
    let amount = parts.next()?.replace(',', "");
    let unit = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let multiplier = match unit {
        "B" => 1_u128,
        "KB" => 1_000,
        "MB" => 1_000_000,
        "GB" => 1_000_000_000,
        _ => return None,
    };
    let (whole, fraction) = amount.split_once('.').unwrap_or((amount.as_str(), ""));
    if fraction.len() > 6 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let whole = whole.parse::<u128>().ok()?;
    let scale = 10_u128.checked_pow(u32::try_from(fraction.len()).ok()?)?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u128>().ok()?
    };
    let bytes = whole
        .checked_mul(multiplier)?
        .checked_add(fraction.checked_mul(multiplier)?.checked_div(scale)?)?;
    u64::try_from(bytes).ok()
}

fn official_fallback(now: DateTime<Utc>) -> CodexDesktopWindowsDownloads {
    let packages = [ClientArchitecture::X64, ClientArchitecture::Arm64]
        .into_iter()
        .map(official_package)
        .collect();
    CodexDesktopWindowsDownloads {
        resolved_at: now,
        cached: false,
        warning: Some(
            "Microsoft Store 离线直链暂不可用，已切换为 OpenAI 官方稳定安装包。".to_owned(),
        ),
        packages,
    }
}

/// 为动态结果中缺失的架构补齐 OpenAI 官方稳定包。
#[must_use]
pub fn complete_with_official_fallback(
    packages: Vec<ClientDownloadPackage>,
) -> (Vec<ClientDownloadPackage>, Option<String>) {
    let mut packages = packages
        .into_iter()
        .map(|package| (package.architecture, package))
        .collect::<BTreeMap<_, _>>();
    let mut missing = Vec::new();
    for architecture in [ClientArchitecture::X64, ClientArchitecture::Arm64] {
        if let std::collections::btree_map::Entry::Vacant(entry) = packages.entry(architecture) {
            missing.push(architecture.as_str());
            entry.insert(official_package(architecture));
        }
    }
    let warning = (!missing.is_empty()).then(|| {
        format!(
            "{} 的 Microsoft Store 临时直链不可用，已切换为 OpenAI 官方稳定安装包。",
            missing.join("、")
        )
    });
    (packages.into_values().collect(), warning)
}

fn official_package(architecture: ClientArchitecture) -> ClientDownloadPackage {
    let (download_url, file_name) = match architecture {
        ClientArchitecture::X64 => (OFFICIAL_X64_URL, "ChatGPT-x64.msix"),
        ClientArchitecture::Arm64 => (OFFICIAL_ARM64_URL, "ChatGPT-arm64.msix"),
    };
    ClientDownloadPackage {
        architecture,
        source: ClientDownloadSource::OfficialOpenAi,
        version: None,
        file_name: file_name.to_owned(),
        size_bytes: None,
        download_url: download_url.to_owned(),
        expires_at: None,
    }
}
