use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::Utc;
use gateway_admin::model::{
    AdminError,
    plugins::{
        PluginSource,
        distribution::{
            DownloadPurpose, GithubReleaseQuery, PluginDistributionEgress, PluginRelease,
            ReleaseAsset, SourceCredential,
        },
    },
};
use reqwest::Url;
use serde::Deserialize;
use tokio::sync::Mutex;

use super::{
    HttpPluginDistribution, MAX_METADATA_BYTES, credential_ids, http, source_error, validate_digest,
};

#[derive(Clone)]
pub(super) struct CachedRelease {
    result: Result<ResolvedRelease, AdminError>,
    until: Instant,
    fetched_at: Instant,
}

#[derive(Clone)]
pub(super) struct ResolvedRelease {
    pub(super) view: PluginRelease,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    name: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Clone, Deserialize)]
struct GitHubAsset {
    id: u64,
    name: String,
    size: u64,
    digest: Option<String>,
}

impl HttpPluginDistribution {
    pub(super) async fn release(
        &self,
        mut query: GithubReleaseQuery,
        credentials: &[SourceCredential],
        egress: Option<&PluginDistributionEgress>,
        refresh: bool,
    ) -> Result<ResolvedRelease, AdminError> {
        let requested_at = Instant::now();
        validate_repository(&query.repository)?;
        query.repository.make_ascii_lowercase();
        let url = self.release_url(&query)?;
        let key = format!(
            "{}:{}:{}:{}",
            url,
            query.allow_prerelease,
            http::scope_key(&url, DownloadPurpose::Metadata, credentials)?,
            egress.map_or_else(|| "direct".to_owned(), PluginDistributionEgress::cache_key)
        );
        let slot = {
            let mut entries = self.queries.lock().await;
            if entries.len() >= 64 {
                entries.retain(|_, slot| {
                    Arc::strong_count(slot) > 1
                        || slot.try_lock().map_or(true, |entry| {
                            entry
                                .as_ref()
                                .is_some_and(|entry| entry.until > Instant::now())
                        })
                });
            }
            if entries.len() >= 64 && !entries.contains_key(&key) {
                return Err(AdminError::unavailable("插件来源查询缓存已满，请稍后重试"));
            }
            entries
                .entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(None)))
                .clone()
        };
        tokio::time::timeout(Duration::from_secs(60), async {
            // 同一来源共用锁和结果；取消请求释放锁，下一位查询者可重新发起。
            let mut cached = slot.lock().await;
            // 显式查询刷新成功结果；等待同一次请求的调用者仍共享结果，失败保留短缓存限流。
            if let Some(entry) = cached.as_ref().filter(|entry| {
                entry.until > Instant::now()
                    && (!refresh || entry.fetched_at >= requested_at || entry.result.is_err())
            }) {
                return entry.result.clone();
            }
            let _permit = self
                .concurrency
                .acquire()
                .await
                .map_err(|_| source_error())?;
            let result = self.fetch_release(url, query, credentials, egress).await;
            let ttl = if result.is_ok() {
                Duration::from_secs(3600)
            } else {
                Duration::from_secs(30)
            };
            let fetched_at = Instant::now();
            *cached = Some(CachedRelease {
                result: result.clone(),
                until: fetched_at + ttl,
                fetched_at,
            });
            result
        })
        .await
        .map_err(|_| AdminError::bad_gateway("GitHub Release 查询超时"))?
    }

    fn release_url(&self, query: &GithubReleaseQuery) -> Result<Url, AdminError> {
        let mut url = self.api_base.clone();
        {
            let mut path = url.path_segments_mut().map_err(|_| source_error())?;
            path.pop_if_empty().push("repos");
            for segment in query.repository.split('/') {
                path.push(segment);
            }
            path.push("releases");
            if let Some(tag) = &query.tag {
                if tag.is_empty() || tag.len() > 256 || tag.chars().any(char::is_control) {
                    return Err(AdminError::invalid("GitHub tag 不合法"));
                }
                path.push("tags").push(tag);
            } else {
                path.push("latest");
            }
        }
        Ok(url)
    }

    async fn fetch_release(
        &self,
        url: Url,
        query: GithubReleaseQuery,
        credentials: &[SourceCredential],
        egress: Option<&PluginDistributionEgress>,
    ) -> Result<ResolvedRelease, AdminError> {
        let bytes = self
            .downloads
            .get(
                url,
                DownloadPurpose::Metadata,
                credentials,
                MAX_METADATA_BYTES,
                true,
                egress,
            )
            .await?;
        let release: GitHubRelease = serde_json::from_slice(&bytes).map_err(|_| source_error())?;
        if release.draft
            || (release.prerelease && !query.allow_prerelease)
            || release.tag_name.is_empty()
            || release.tag_name.len() > 256
            || release.tag_name.chars().any(char::is_control)
            || release.assets.len() > 256
            || query
                .tag
                .as_ref()
                .is_some_and(|tag| *tag != release.tag_name)
        {
            return Err(AdminError::invalid(
                "Release 的版本或发布状态与选择不符；预发行版需要显式选择",
            ));
        }
        let mut names = std::collections::BTreeSet::new();
        for asset in &release.assets {
            if asset.id == 0
                || asset.name.is_empty()
                || asset.name.len() > 256
                || asset.name.contains(['/', '\\'])
                || asset.name.chars().any(char::is_control)
                || !names.insert(&asset.name)
            {
                return Err(AdminError::invalid("GitHub Release 产物描述不合法"));
            }
        }
        let queried_at = Utc::now();
        Ok(ResolvedRelease {
            view: PluginRelease {
                repository: query.repository,
                tag: release.tag_name,
                name: release.name.unwrap_or_default().chars().take(256).collect(),
                prerelease: release.prerelease,
                assets: release
                    .assets
                    .iter()
                    .map(|asset| ReleaseAsset {
                        name: asset.name.clone(),
                        size: asset.size,
                        sha256: asset_digest(asset),
                    })
                    .collect(),
                queried_at,
                expires_at: queried_at + chrono::Duration::hours(1),
            },
            assets: release.assets,
        })
    }

    pub(super) async fn resolve_asset(
        &self,
        query: GithubReleaseQuery,
        asset: String,
        expected: Option<String>,
        credentials: &[SourceCredential],
        egress: Option<&PluginDistributionEgress>,
    ) -> Result<(Url, Option<String>, PluginSource, bool), AdminError> {
        let release = self.release(query, credentials, egress, false).await?;
        let selected = release
            .assets
            .iter()
            .find(|candidate| candidate.name == asset)
            .ok_or_else(|| AdminError::not_found("Release 不包含指定产物"))?;
        if selected.size == 0 || selected.size > super::MAX_ARCHIVE_BYTES as u64 {
            return Err(AdminError::invalid("插件包大小不合法"));
        }
        let source_digest = asset_digest(selected);
        let digest = if let Some(expected) = expected {
            validate_digest(&expected)?;
            if source_digest
                .as_ref()
                .is_some_and(|source| *source != expected)
            {
                return Err(AdminError::invalid("输入摘要与 GitHub 产物摘要不一致"));
            }
            Some(expected)
        } else if let Some(digest) = source_digest {
            Some(digest)
        } else if let Some(checksums) = release
            .assets
            .iter()
            .find(|asset| asset.name == "checksums.txt")
        {
            let _permit = self
                .concurrency
                .acquire()
                .await
                .map_err(|_| source_error())?;
            let bytes = self
                .downloads
                .get(
                    self.asset_url(&release.view.repository, checksums.id)?,
                    DownloadPurpose::Artifact,
                    credentials,
                    MAX_METADATA_BYTES,
                    true,
                    egress,
                )
                .await?;
            Some(checksum_for(&bytes, &asset)?)
        } else {
            None
        };
        let url = self.asset_url(&release.view.repository, selected.id)?;
        let source = PluginSource::Github {
            repository: release.view.repository,
            tag: release.view.tag,
            asset,
            credential_ids: credential_ids(credentials),
            outbound_proxy: egress.map(|egress| egress.source.clone()),
        };
        Ok((url, digest, source, true))
    }

    fn asset_url(&self, repository: &str, id: u64) -> Result<Url, AdminError> {
        self.api_base
            .join(&format!("repos/{repository}/releases/assets/{id}"))
            .map_err(|_| source_error())
    }
}

fn asset_digest(asset: &GitHubAsset) -> Option<String> {
    asset
        .digest
        .as_deref()
        .and_then(|value| value.strip_prefix("sha256:"))
        .filter(|digest| validate_digest(digest).is_ok())
        .map(str::to_owned)
}

pub(super) fn validate_repository(repository: &str) -> Result<(), AdminError> {
    let parts: Vec<_> = repository.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || part.len() > 100
                || *part == "."
                || *part == ".."
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        })
    {
        return Err(AdminError::invalid("GitHub 仓库需要 owner/repository 格式"));
    }
    Ok(())
}

fn checksum_for(bytes: &[u8], asset: &str) -> Result<String, AdminError> {
    let text = std::str::from_utf8(bytes).map_err(|_| source_error())?;
    let mut found = None;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(digest) = fields.next() else {
            continue;
        };
        let Some(name) = fields.next() else { continue };
        if name.trim_start_matches('*') == asset {
            validate_digest(digest)?;
            if fields.next().is_some() || found.replace(digest.to_owned()).is_some() {
                return Err(AdminError::invalid(
                    "checksums.txt 包含重复或不合法的目标摘要",
                ));
            }
        }
    }
    found.ok_or_else(|| AdminError::invalid("checksums.txt 缺少目标产物摘要"))
}
