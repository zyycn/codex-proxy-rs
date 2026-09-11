//! Host 为出站代理连通性测试提供的探测能力。

use async_trait::async_trait;
use gateway_core::account::OutboundProxy;

use crate::model::proxies::OutboundProxyTestReport;

/// 通过指定代理探测目标 URL 的连通性；实现唯一归 gateway-host。
///
/// 探测不产生领域错误：失败以脱敏后的 [`OutboundProxyTestReport`] 返回，
/// 错误消息不得包含代理 URL、凭据或目标响应正文。
#[async_trait]
pub trait OutboundProxyProbe: Send + Sync {
    async fn probe(&self, proxy: &OutboundProxy, target_url: &str) -> OutboundProxyTestReport;
}
