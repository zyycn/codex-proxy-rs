//! 出站代理连通性探测；错误消息不回显代理 URL、凭据或响应正文。

use std::time::{Duration, Instant};

use async_trait::async_trait;
use gateway_admin::{model::proxies::OutboundProxyTestReport, ports::proxy::OutboundProxyProbe};
use gateway_core::account::OutboundProxy;

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// 每次探测构建独立 reqwest client 的默认实现。
pub(crate) struct ReqwestOutboundProxyProbe;

#[async_trait]
impl OutboundProxyProbe for ReqwestOutboundProxyProbe {
    async fn probe(&self, proxy: &OutboundProxy, target_url: &str) -> OutboundProxyTestReport {
        let started = Instant::now();
        let report = |success, status_code, error| OutboundProxyTestReport {
            success,
            latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            status_code,
            target_url: target_url.to_owned(),
            error,
        };
        let proxy_setting = match reqwest::Proxy::all(proxy.expose_url()) {
            Ok(setting) => setting,
            Err(_) => return report(false, None, Some("代理 URL 无法用于建立连接".to_owned())),
        };
        let client = match reqwest::Client::builder()
            .no_proxy()
            .proxy(proxy_setting)
            .timeout(PROBE_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
        {
            Ok(client) => client,
            Err(_) => return report(false, None, Some("创建探测客户端失败".to_owned())),
        };
        match client.get(target_url).send().await {
            // 任何 HTTP 状态都证明代理链路可达；目标返回 4xx/5xx 不算失败。
            Ok(response) => report(true, Some(response.status().as_u16()), None),
            Err(error) => report(false, None, Some(sanitized_probe_error(&error))),
        }
    }
}

/// 归类 reqwest 错误为不含 URL/凭据的稳定中文描述。
fn sanitized_probe_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "连接超时".to_owned()
    } else if error.is_connect() {
        "无法建立到代理或目标的连接".to_owned()
    } else if error.is_request() {
        "代理拒绝了探测请求".to_owned()
    } else {
        "探测请求失败".to_owned()
    }
}
