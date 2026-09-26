//! 插件来源查询和受控下载；不解析插件包，不持有数据库或管理事务。

mod github;
mod http;

use std::{collections::HashMap, sync::Arc, time::Duration};

use crate::outbound::{HttpClient, NetworkPolicy};
use async_trait::async_trait;
use gateway_admin::{
    model::{
        AdminError,
        plugins::{
            PluginSource,
            distribution::{
                DownloadedPlugin, GithubReleaseQuery, PluginDistributionEgress, PluginRelease,
                RemotePluginLocation, SourceCredential,
            },
        },
    },
    ports::plugins::PluginDistribution,
};
use reqwest::Url;
use sha2::{Digest as _, Sha256};
use tokio::sync::{Mutex, Semaphore};

use self::{github::CachedRelease, http::Downloads};

const MAX_ARCHIVE_BYTES: usize = 32 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 1024 * 1024;

/// 插件分发服务复用宿主受控 HTTP；缓存与限流不会跨出站身份共享。
pub struct HttpPluginDistribution {
    downloads: Downloads,
    api_base: Url,
    queries: Mutex<HashMap<String, Arc<Mutex<Option<CachedRelease>>>>>,
    concurrency: Semaphore,
}

impl HttpPluginDistribution {
    pub fn new(http: Arc<HttpClient>) -> Result<Self, AdminError> {
        Self::with_transport("https://api.github.com/", http, source_network()?)
    }

    /// 测试可注入受限网络；生产来源由管理员显式配置，因此保持既有内外网可达范围。
    pub fn with_transport(
        api_base: &str,
        http: Arc<HttpClient>,
        network: NetworkPolicy,
    ) -> Result<Self, AdminError> {
        let api_base = http::source_url(api_base)?;
        Ok(Self {
            downloads: Downloads::new(http, network),
            api_base,
            queries: Mutex::new(HashMap::new()),
            concurrency: Semaphore::new(2),
        })
    }

    async fn download_inner(
        &self,
        location: RemotePluginLocation,
        credentials: Vec<SourceCredential>,
        egress: Option<&PluginDistributionEgress>,
    ) -> Result<DownloadedPlugin, AdminError> {
        let (url, expected, source, github_api) = match location {
            RemotePluginLocation::Url { url, sha256 } => {
                if let Some(digest) = &sha256 {
                    validate_digest(digest)?;
                }
                let parsed = http::source_url(&url)?;
                (
                    parsed,
                    sha256,
                    PluginSource::Url {
                        url,
                        credential_ids: credential_ids(&credentials),
                        outbound_proxy: egress.map(|egress| egress.source.clone()),
                    },
                    false,
                )
            }
            RemotePluginLocation::Github {
                repository,
                tag,
                asset,
                allow_prerelease,
                sha256,
            } => {
                self.resolve_asset(
                    GithubReleaseQuery {
                        repository,
                        tag: Some(tag),
                        allow_prerelease,
                    },
                    asset,
                    sha256,
                    &credentials,
                    egress,
                )
                .await?
            }
        };
        let _permit = self
            .concurrency
            .acquire()
            .await
            .map_err(|_| source_error())?;
        let archive = self
            .downloads
            .get(
                url,
                gateway_admin::model::plugins::distribution::DownloadPurpose::Artifact,
                &credentials,
                MAX_ARCHIVE_BYTES,
                github_api,
                egress,
            )
            .await?;
        let sha256 = hex::encode(Sha256::digest(&archive));
        if expected.is_some_and(|expected| sha256 != expected) {
            return Err(AdminError::invalid("插件下载包的 SHA-256 与来源预期不一致"));
        }
        Ok(DownloadedPlugin {
            archive: archive.into(),
            sha256,
            source,
        })
    }
}

#[async_trait]
impl PluginDistribution for HttpPluginDistribution {
    fn validate_source(
        &self,
        source: &gateway_admin::model::plugins::distribution::PluginUpdateSource,
    ) -> Result<(), AdminError> {
        use gateway_admin::model::plugins::distribution::PluginUpdateSource;
        match source {
            PluginUpdateSource::Builtin | PluginUpdateSource::Upload => Ok(()),
            PluginUpdateSource::Url { url } => http::source_url(url).map(|_| ()),
            PluginUpdateSource::Github { repository } => github::validate_repository(repository),
        }
    }
    fn validate_credential(&self, credential: &SourceCredential) -> Result<(), AdminError> {
        http::validate_credential(credential)
    }

    async fn query_release(
        &self,
        query: GithubReleaseQuery,
        credentials: Vec<SourceCredential>,
        egress: Option<PluginDistributionEgress>,
    ) -> Result<PluginRelease, AdminError> {
        Ok(self
            .release(query, &credentials, egress.as_ref(), true)
            .await?
            .view)
    }

    async fn download(
        &self,
        location: RemotePluginLocation,
        credentials: Vec<SourceCredential>,
        egress: Option<PluginDistributionEgress>,
    ) -> Result<DownloadedPlugin, AdminError> {
        tokio::time::timeout(
            Duration::from_secs(120),
            self.download_inner(location, credentials, egress.as_ref()),
        )
        .await
        .map_err(|_| AdminError::bad_gateway("插件下载超时，请重试"))?
    }
}

fn source_network() -> Result<NetworkPolicy, AdminError> {
    NetworkPolicy::new(&["0.0.0.0/0".into(), "::/0".into()]).map_err(|_| source_error())
}

fn credential_ids(credentials: &[SourceCredential]) -> Vec<String> {
    credentials
        .iter()
        .map(|credential| credential.info.id.clone())
        .collect()
}

fn source_error() -> AdminError {
    AdminError::bad_gateway("插件来源请求失败，请检查来源与下载授权后重试")
}

fn validate_digest(value: &str) -> Result<(), AdminError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(AdminError::invalid("需要小写十六进制 SHA-256 摘要"))
    }
}
