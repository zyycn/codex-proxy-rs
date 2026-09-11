//! Bounded egress checks using the same explicit proxy protocols as provider requests.

use std::{
    net::IpAddr,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use gateway_admin::{model::proxies::ProxyTestResult, ports::proxy::ProxyProbe};
use gateway_core::account::OutboundProxy;
use serde::Deserialize;

pub struct HttpProxyProbe {
    endpoint: String,
}

impl Default for HttpProxyProbe {
    fn default() -> Self {
        Self::new("https://api.ipify.org?format=json")
    }
}

impl HttpProxyProbe {
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }

    async fn exit_ip(&self, proxy: &OutboundProxy) -> Result<IpAddr, &'static str> {
        let proxy = reqwest::Proxy::all(proxy.expose_url()).map_err(|_| "代理地址不合法")?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .proxy(proxy)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(12))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "无法创建代理连接")?;
        let mut response = client.get(&self.endpoint).send().await.map_err(|error| {
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

#[async_trait]
impl ProxyProbe for HttpProxyProbe {
    async fn test(&self, proxy: &OutboundProxy) -> ProxyTestResult {
        let started = Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(15), self.exit_ip(proxy)).await;
        let result = result.unwrap_or(Err("代理连接超时"));
        ProxyTestResult {
            success: result.is_ok(),
            latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            exit_ip: result.as_ref().ok().copied(),
            message: result.map_or_else(str::to_owned, |_| "连接成功".to_owned()),
        }
    }
}
