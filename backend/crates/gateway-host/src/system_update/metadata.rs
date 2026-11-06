//! 从固定发布索引和精确版本入口读取有界元数据

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use gateway_admin::model::system::SystemUpdateChannel as Channel;
use reqwest::{Client, StatusCode, Url};
use semver::Version;
use tokio::sync::{Mutex, OnceCell};
use tokio::time::Instant;
use yaml_rust2::{
    Yaml, YamlLoader,
    parser::{Event, Parser},
};

use super::release::{GitHubRelease, channel_accepts, channel_for_version};
use super::{OperationError, SystemUpdateConfig, invalid, upstream};

#[derive(Default)]
pub(super) struct Metadata {
    client: OnceCell<Client>,
    raw_until: Mutex<Option<Cooldown>>,
    api_until: Mutex<Option<Cooldown>>,
}

#[derive(Clone, Copy)]
struct Cooldown {
    until: Instant,
    retry_at: DateTime<Utc>,
}

impl Cooldown {
    fn warning(self) -> OperationError {
        upstream("GitHub 暂时限制了更新检查").with_retry_at(self.retry_at)
    }
}

impl Metadata {
    pub(super) async fn index(
        &self,
        config: &SystemUpdateConfig,
    ) -> Result<Arc<Yaml>, OperationError> {
        let repo = repository(config)?;
        let mut url =
            Url::parse(&config.github_api_base).map_err(|_| invalid("发行查询地址无效"))?;
        if url.host_str() == Some("api.github.com") {
            url = Url::parse("https://raw.githubusercontent.com").expect("固定的 GitHub raw 源");
        }
        // 配置校验只额外接受回环源，测试时索引和 Release 均留在本机
        url.set_path(&format!("/{repo}/refs/heads/main/release/version.yaml"));
        let bytes = self
            .read(url.as_str(), &self.raw_until, 8192, false)
            .await?
            .ok_or_else(|| upstream("更新仓库缺少发布索引"))?;
        let source = std::str::from_utf8(&bytes).map_err(|_| upstream("发布索引格式无效"))?;
        let bad = || upstream("发布索引格式无效");
        // 远程 YAML 不使用别名，限制嵌套后再构造节点，避免小响应放大内存
        let mut parser = Parser::new_from_str(source);
        let mut depth = 0;
        loop {
            match parser.next_token().map_err(|_| bad())?.0 {
                Event::StreamEnd => break,
                Event::Alias(_) => return Err(bad()),
                Event::MappingStart(..) | Event::SequenceStart(..) => {
                    depth += 1;
                    if depth > 8 {
                        return Err(bad());
                    }
                }
                Event::MappingEnd | Event::SequenceEnd => depth -= 1,
                _ => {}
            }
        }
        let mut docs = YamlLoader::load_from_str(source).map_err(|_| bad())?;
        if docs.len() != 1 || docs[0]["published"].as_hash().is_none() {
            return Err(upstream("发布索引缺少有效的 published 通道表"));
        }
        Ok(Arc::new(docs.remove(0)))
    }

    pub(super) async fn release(
        &self,
        config: &SystemUpdateConfig,
        version: &Version,
    ) -> Result<Option<Arc<GitHubRelease>>, OperationError> {
        let mut url =
            Url::parse(&config.github_api_base).map_err(|_| invalid("发行查询地址无效"))?;
        let tag = format!("v{version}");
        url.path_segments_mut()
            .map_err(|()| invalid("发行查询地址无效"))?
            .pop_if_empty()
            .extend(repository(config)?.split('/'))
            .extend(["releases", "tags", &tag]);
        let Some(bytes) = self
            .read(url.as_str(), &self.api_until, 4 * 1024 * 1024, true)
            .await?
        else {
            // 当前运行版本可以没有公开说明；索引承诺的升级目标缺失属于短期查询失败
            if config.version.trim_start_matches('v') == version.to_string() {
                return Ok(None);
            }
            return Err(upstream("发布索引指向的 Release 不存在"));
        };
        let release: GitHubRelease = serde_json::from_slice(&bytes)
            .map_err(|error| upstream("GitHub 发行信息格式无效").with_source(error))?;
        if release.tag_name != tag || release.draft || release.prerelease == version.pre.is_empty()
        {
            return Err(upstream("GitHub 发行身份与发布索引不符"));
        }
        Ok(Some(Arc::new(release)))
    }

    async fn read(
        &self,
        url: &str,
        cooldown: &Mutex<Option<Cooldown>>,
        limit: usize,
        api: bool,
    ) -> Result<Option<Vec<u8>>, OperationError> {
        if let Some(active) = cooldown
            .lock()
            .await
            .filter(|active| active.until > Instant::now())
        {
            return Err(active.warning());
        }
        let client = self
            .client
            .get_or_try_init(|| async {
                Client::builder()
                    .user_agent("codex-proxy-rs")
                    .timeout(Duration::from_secs(10))
                    .connect_timeout(Duration::from_secs(5))
                    .redirect(reqwest::redirect::Policy::none())
                    .retry(reqwest::retry::never())
                    .build()
            })
            .await
            .map_err(|e| super::error::http("无法初始化发行查询连接", e))?;
        let request = client.get(url);
        let mut response = if api {
            request.header("Accept", "application/vnd.github+json")
        } else {
            request
        }
        .send()
        .await
        .map_err(|e| super::error::http("无法查询 GitHub 发行版本", e))?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            let headers = response.headers();
            let header = |name: &str| headers.get(name).and_then(|h| h.to_str().ok());
            let retry = header("retry-after").and_then(|s| s.parse::<u64>().ok());
            let exhausted = header("x-ratelimit-remaining") == Some("0");
            if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::FORBIDDEN {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let reset = if exhausted {
                    header("x-ratelimit-reset")
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(|v| v.saturating_sub(now))
                } else {
                    None
                };
                let seconds = retry
                    .into_iter()
                    .chain(reset)
                    .max()
                    .unwrap_or(60)
                    .clamp(1, 86400);
                let duration = Duration::from_secs(seconds);
                let limit = Cooldown {
                    until: Instant::now() + duration,
                    retry_at: Utc::now() + duration,
                };
                let mut guard = cooldown.lock().await;
                let active = guard.filter(|old| old.until > limit.until).unwrap_or(limit);
                *guard = Some(active);
                let warning = active.warning();
                return Err(match response.error_for_status_ref() {
                    Err(error) => warning.with_source(error.without_url()),
                    Ok(_) => warning,
                });
            }
            return Err(upstream(format!(
                "GitHub 发行查询失败（HTTP {}）",
                status.as_u16()
            )));
        }
        let too_large = || upstream("GitHub 发行元数据超出大小限制");
        if response
            .content_length()
            .is_some_and(|size| size > limit as u64)
        {
            return Err(too_large());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| super::error::http("读取 GitHub 发行元数据失败", e))?
        {
            if chunk.len() > limit - bytes.len() {
                return Err(too_large());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some(bytes))
    }
}

fn repository(config: &SystemUpdateConfig) -> Result<&str, OperationError> {
    config
        .update_repository
        .as_deref()
        .ok_or_else(|| invalid("未配置更新仓库"))
}

pub(super) fn published_version(
    index: &Yaml,
    current: &Version,
    channel: Channel,
) -> Result<Version, OperationError> {
    let line = &index["published"][current.major.to_string().as_str()];
    let node = if channel == Channel::Experimental {
        &line["exp"][format!("{}.{}.{}", current.major, current.minor, current.patch).as_str()]
    } else {
        &line[channel.as_str()]
    };
    let raw = node
        .as_str()
        .ok_or_else(|| upstream("所选更新通道尚未发布"))?;
    let target = Version::parse(raw).map_err(|_| upstream("发布索引中的版本号无效"))?;
    let allowed = if channel == Channel::Experimental {
        channel_for_version(&target) == Some(channel)
            && (current.minor, current.patch) == (target.minor, target.patch)
    } else {
        channel_for_version(&target).is_some_and(|stage| channel_accepts(channel, stage))
    };
    if current.major != target.major || !allowed {
        return Err(upstream("发布索引中的版本不属于所选通道"));
    }
    Ok(target)
}
