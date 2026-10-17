//! 账号出站代理连接与容量配置，诊断及普通管理投影不暴露凭据

use std::fmt;
use url::Url;

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct OutboundProxy {
    url: Url,
    max_websocket_connections: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid outbound proxy; expected http, https, socks5 or socks5h URL with a host and port")]
pub struct InvalidOutboundProxy;

impl OutboundProxy {
    pub fn parse(value: &str) -> Result<Self, InvalidOutboundProxy> {
        if value.len() > 4096 || value.chars().any(char::is_control) {
            return Err(InvalidOutboundProxy);
        }
        let url = Url::parse(value).map_err(|_| InvalidOutboundProxy)?;
        if !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h")
            || url.host_str().is_none()
            || url.port_or_known_default().is_none_or(|port| port == 0)
            || !matches!(url.path(), "" | "/")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(InvalidOutboundProxy);
        }
        Ok(Self {
            url,
            max_websocket_connections: 0,
        })
    }

    /// 此出口的上游 WebSocket 容量，零值表示不限制
    #[must_use]
    pub fn with_max_websocket_connections(mut self, limit: u32) -> Self {
        self.max_websocket_connections = limit;
        self
    }

    #[must_use]
    pub const fn max_websocket_connections(&self) -> u32 {
        self.max_websocket_connections
    }

    #[must_use]
    pub fn expose_url(&self) -> &str {
        self.url.as_str()
    }

    #[must_use]
    pub fn endpoint(&self) -> String {
        let mut url = self.url.clone();
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.to_string()
    }
}

impl fmt::Debug for OutboundProxy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OutboundProxy(<redacted>)")
    }
}
