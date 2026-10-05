//! 使用与 Provider 请求一致的显式代理协议，执行有超时和响应大小限制的出口测试

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use gateway_admin::{
    model::proxies::{ProxyLocationDetection, ProxyTestResult},
    ports::proxy::ProxyProbe,
};
use gateway_core::account::{OutboundProxy, RequestLocation};
use serde::Deserialize;

enum ProbeStrategy {
    Single(String),
    Dual {
        ipv4_endpoint: String,
        ipv6_endpoint: String,
    },
}

pub struct HttpProxyProbe {
    strategy: ProbeStrategy,
    location_endpoint: String,
    build_client: Arc<ProxyClientBuilder>,
}

type ProxyClientBuilder =
    dyn Fn(reqwest::ClientBuilder) -> Result<reqwest::Client, &'static str> + Send + Sync;

impl Default for HttpProxyProbe {
    fn default() -> Self {
        // 分别向 IPv4 和 IPv6 专用端点并发探测，以获取真实的双栈出口地址
        Self::new_dual(
            "https://api.ipify.org?format=json",
            "https://api6.ipify.org?format=json",
        )
    }
}

impl HttpProxyProbe {
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            location_endpoint: "https://ipwho.is/".to_owned(),
            strategy: ProbeStrategy::Single(endpoint.into()),
            build_client: Arc::new(|builder| builder.build().map_err(|_| "无法创建代理连接")),
        }
    }

    #[must_use]
    pub fn new_dual(ipv4_endpoint: impl Into<String>, ipv6_endpoint: impl Into<String>) -> Self {
        Self {
            location_endpoint: "https://ipwho.is/".to_owned(),
            strategy: ProbeStrategy::Dual {
                ipv4_endpoint: ipv4_endpoint.into(),
                ipv6_endpoint: ipv6_endpoint.into(),
            },
            build_client: Arc::new(|builder| builder.build().map_err(|_| "无法创建代理连接")),
        }
    }

    /// 由组合根注入与 Provider 请求一致的证书信任策略
    #[must_use]
    pub fn with_client_builder<E>(
        mut self,
        build: impl Fn(reqwest::ClientBuilder) -> Result<reqwest::Client, E> + Send + Sync + 'static,
    ) -> Self {
        self.build_client = Arc::new(move |builder| {
            build(builder).map_err(|_| "无法创建代理连接，请检查证书信任配置")
        });
        self
    }

    async fn exit_ip_at(
        &self,
        proxy: &OutboundProxy,
        endpoint: &str,
    ) -> Result<IpAddr, &'static str> {
        let proxy = reqwest::Proxy::all(proxy.expose_url()).map_err(|_| "代理地址不合法")?;
        let builder = reqwest::Client::builder()
            .no_proxy()
            .proxy(proxy)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(12))
            .redirect(reqwest::redirect::Policy::none());
        let client = (self.build_client)(builder)?;
        let mut response = client.get(endpoint).send().await.map_err(|error| {
            if error.is_timeout() {
                "代理连接超时"
            } else {
                "代理连接失败，请检查地址、认证和网络"
            }
        })?;
        if !response.status().is_success() {
            return Err(
                if response.status() == reqwest::StatusCode::PROXY_AUTHENTICATION_REQUIRED {
                    "代理认证失败"
                } else {
                    "出口检测服务返回错误状态"
                },
            );
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "出口检测响应读取失败")?
        {
            if body.len() + chunk.len() > 1024 {
                return Err("出口检测响应过大");
            }
            body.extend_from_slice(&chunk);
        }
        #[derive(Deserialize)]
        struct Response {
            ip: IpAddr,
        }
        serde_json::from_slice::<Response>(&body)
            .map(|response| response.ip)
            .map_err(|_| "出口检测响应不合法")
    }
}

impl HttpProxyProbe {
    async fn test_connection(&self, proxy: &OutboundProxy) -> ProxyTestResult {
        let started = Instant::now();
        let timeout_limit = Duration::from_secs(15);

        match &self.strategy {
            ProbeStrategy::Single(endpoint) => {
                let result =
                    tokio::time::timeout(timeout_limit, self.exit_ip_at(proxy, endpoint)).await;
                let result = result.unwrap_or(Err("代理连接超时"));
                let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

                match result {
                    Ok(ip) => {
                        let (exit_ipv4, exit_ipv6) = match ip {
                            IpAddr::V4(v4) => (Some(v4), None),
                            IpAddr::V6(v6) => (None, Some(v6)),
                        };
                        ProxyTestResult {
                            location: ProxyLocationDetection::NotRequested,
                            success: true,
                            latency_ms,
                            exit_ip: Some(ip),
                            exit_ipv4,
                            exit_ipv6,
                            message: "连接成功".to_owned(),
                        }
                    }
                    Err(err) => ProxyTestResult {
                        location: ProxyLocationDetection::NotRequested,
                        success: false,
                        latency_ms,
                        exit_ip: None,
                        exit_ipv4: None,
                        exit_ipv6: None,
                        message: err.to_owned(),
                    },
                }
            }
            ProbeStrategy::Dual {
                ipv4_endpoint,
                ipv6_endpoint,
            } => {
                let probe_dual = async {
                    tokio::join!(
                        self.exit_ip_at(proxy, ipv4_endpoint),
                        self.exit_ip_at(proxy, ipv6_endpoint),
                    )
                };
                let result = tokio::time::timeout(timeout_limit, probe_dual).await;
                let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

                match result {
                    Ok((res_v4, res_v6)) => {
                        let exit_ipv4: Option<Ipv4Addr> = match res_v4 {
                            Ok(IpAddr::V4(v4)) => Some(v4),
                            _ => None,
                        };
                        let exit_ipv6: Option<Ipv6Addr> = match res_v6 {
                            Ok(IpAddr::V6(v6)) => Some(v6),
                            _ => None,
                        };

                        if exit_ipv4.is_some() && exit_ipv6.is_some() {
                            ProxyTestResult {
                                location: ProxyLocationDetection::NotRequested,
                                success: true,
                                latency_ms,
                                exit_ip: exit_ipv4.map(IpAddr::V4),
                                exit_ipv4,
                                exit_ipv6,
                                message: "连接成功（双栈可用）".to_owned(),
                            }
                        } else if let Some(v4) = exit_ipv4 {
                            ProxyTestResult {
                                location: ProxyLocationDetection::NotRequested,
                                success: true,
                                latency_ms,
                                exit_ip: Some(IpAddr::V4(v4)),
                                exit_ipv4: Some(v4),
                                exit_ipv6: None,
                                message: "连接成功（仅 IPv4）".to_owned(),
                            }
                        } else if let Some(v6) = exit_ipv6 {
                            ProxyTestResult {
                                location: ProxyLocationDetection::NotRequested,
                                success: true,
                                latency_ms,
                                exit_ip: Some(IpAddr::V6(v6)),
                                exit_ipv4: None,
                                exit_ipv6: Some(v6),
                                message: "连接成功（仅 IPv6）".to_owned(),
                            }
                        } else {
                            let message = res_v4
                                .err()
                                .or(res_v6.err())
                                .unwrap_or("代理连接失败")
                                .to_owned();
                            ProxyTestResult {
                                location: ProxyLocationDetection::NotRequested,
                                success: false,
                                latency_ms,
                                exit_ip: None,
                                exit_ipv4: None,
                                exit_ipv6: None,
                                message,
                            }
                        }
                    }
                    Err(_) => ProxyTestResult {
                        location: ProxyLocationDetection::NotRequested,
                        success: false,
                        latency_ms,
                        exit_ip: None,
                        exit_ipv4: None,
                        exit_ipv6: None,
                        message: "代理连接超时".to_owned(),
                    },
                }
            }
        }
    }
}

impl HttpProxyProbe {
    /// 显式注入地理位置服务地址，便于部署适配与隔离网络验证
    #[must_use]
    pub fn with_location_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.location_endpoint = endpoint.into();
        self
    }

    async fn location_for_ip(
        &self,
        proxy: &OutboundProxy,
        ip: IpAddr,
    ) -> Result<RequestLocation, &'static str> {
        // 出口 IP 已由代理探测确认；按这个固定 IP 从服务端查询位置，避免代理出口屏蔽位置服务
        // 部署环境无法直连位置服务时仍尝试原代理路径
        match self.lookup_location(ip, None).await {
            Ok(location) => Ok(location),
            Err(direct_error) => self
                .lookup_location(ip, Some(proxy))
                .await
                .or(Err(direct_error)),
        }
    }

    async fn lookup_location(
        &self,
        ip: IpAddr,
        proxy: Option<&OutboundProxy>,
    ) -> Result<RequestLocation, &'static str> {
        let timeout = if proxy.is_some() {
            Duration::from_secs(4)
        } else {
            Duration::from_secs(3)
        };
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(timeout)
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none());
        if let Some(proxy) = proxy {
            builder = builder
                .proxy(reqwest::Proxy::all(proxy.expose_url()).map_err(|_| "代理地址不合法")?);
        }
        let client = (self.build_client)(builder)?;
        // 查询已检测到的具体出口，不能再次查询“我的 IP”，轮换代理可能换到另一个出口
        let url = format!("{}/{ip}", self.location_endpoint.trim_end_matches('/'));
        let mut response = client
            .get(url)
            .query(&[("fields", "ip,success,country_code,region,city,timezone.id")])
            .send()
            .await
            .map_err(|_| "位置查询失败，请手动重试")?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err("位置查询服务限流，请稍后手动重试");
        }
        if !response.status().is_success() {
            return Err("位置查询服务返回错误状态");
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "位置查询响应读取失败")?
        {
            if body.len() + chunk.len() > 8192 {
                return Err("位置查询响应过大");
            }
            body.extend_from_slice(&chunk);
        }
        #[derive(Deserialize)]
        struct Timezone {
            id: String,
        }
        #[derive(Deserialize)]
        struct Response {
            success: bool,
            ip: Option<IpAddr>,
            country_code: Option<String>,
            region: Option<String>,
            city: Option<String>,
            timezone: Option<Timezone>,
        }
        let result: Response = serde_json::from_slice(&body).map_err(|_| "位置查询响应不合法")?;
        if !result.success || result.ip != Some(ip) {
            return Err("位置查询未返回对应出口的信息");
        }
        let incomplete = "出口地区或 IANA 时区信息不完整";
        RequestLocation {
            country: result.country_code.ok_or(incomplete)?,
            region: result.region.ok_or(incomplete)?,
            city: result.city.ok_or(incomplete)?,
            timezone: result
                .timezone
                .ok_or(incomplete)?
                .id
                .parse()
                .map_err(|_| incomplete)?,
        }
        .normalized()
        .map_err(|_| incomplete)
    }

    async fn detect_location(
        &self,
        proxy: &OutboundProxy,
        result: &ProxyTestResult,
    ) -> ProxyLocationDetection {
        let lookup = async |ip: Option<IpAddr>| match ip {
            Some(ip) => self.location_for_ip(proxy, ip).await.map(Some),
            None => Ok(None),
        };
        let (v4, v6) = tokio::join!(
            lookup(result.exit_ipv4.map(IpAddr::V4)),
            lookup(result.exit_ipv6.map(IpAddr::V6))
        );
        match (v4, v6) {
            (Ok(Some(v4)), Ok(Some(v6))) if v4.timezone != v6.timezone => {
                ProxyLocationDetection::Conflict
            }
            (Ok(Some(location)), Ok(_)) | (Ok(None), Ok(Some(location))) => {
                ProxyLocationDetection::Detected { location }
            }
            (Err(message), _) | (_, Err(message)) => ProxyLocationDetection::Failed {
                message: message.to_owned(),
            },
            (Ok(None), Ok(None)) => ProxyLocationDetection::Failed {
                message: "未获取到出口 IP".to_owned(),
            },
        }
    }
}

#[async_trait]
impl ProxyProbe for HttpProxyProbe {
    async fn test(&self, proxy: &OutboundProxy, detect_location: bool) -> ProxyTestResult {
        let mut result = self.test_connection(proxy).await;
        if detect_location {
            result.location = if result.success {
                tokio::time::timeout(Duration::from_secs(7), self.detect_location(proxy, &result))
                    .await
                    .unwrap_or_else(|_| ProxyLocationDetection::Failed {
                        message: "位置查询超时，请手动重试".to_owned(),
                    })
            } else {
                ProxyLocationDetection::Failed {
                    message: "未获取到出口 IP，时区未更新".to_owned(),
                }
            };
        }
        result
    }
}
