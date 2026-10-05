//! 受管出站请求的 DNS 解析、地址分类与网络范围校验

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use futures::future::BoxFuture;
use ipnet::IpNet;

use super::{HttpError, HttpErrorKind};

/// 默认仅允许公网；额外地址段来自明确授权，不由请求或 DNS 回应扩大
#[derive(Clone, Default)]
pub struct NetworkPolicy {
    additional: Vec<IpNet>,
    unrestricted: bool,
}

impl NetworkPolicy {
    /// 完整信任的宿主调用方可以访问任意地址；仍使用相同的 DNS 与传输实现
    #[must_use]
    pub const fn unrestricted() -> Self {
        Self {
            additional: Vec::new(),
            unrestricted: true,
        }
    }

    pub fn new(networks: &[String]) -> Result<Self, HttpError> {
        if networks.len() > 32 {
            return Err(HttpError::invalid("network limits"));
        }
        let additional = networks
            .iter()
            .map(|network| {
                network
                    .parse()
                    .map_err(|_| HttpError::invalid("network range"))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            additional,
            unrestricted: false,
        })
    }

    #[must_use]
    pub fn permits(&self, address: IpAddr) -> bool {
        let address = match address {
            IpAddr::V6(address) => address
                .to_ipv4_mapped()
                .map_or(IpAddr::V6(address), IpAddr::V4),
            other => other,
        };
        self.unrestricted
            || public_address(address)
            || self
                .additional
                .iter()
                .any(|network| network.contains(&address))
    }
}

/// DNS 只产出候选地址；连接层只使用通过授权检查的地址，不再次解析目标域名
pub trait DnsResolver: Send + Sync {
    fn resolve(
        &self,
        host: String,
        port: u16,
    ) -> BoxFuture<'static, std::io::Result<Vec<SocketAddr>>>;
}

pub(super) struct SystemResolver;

impl DnsResolver for SystemResolver {
    fn resolve(
        &self,
        host: String,
        port: u16,
    ) -> BoxFuture<'static, std::io::Result<Vec<SocketAddr>>> {
        Box::pin(async move {
            let addresses = tokio::net::lookup_host((host, port)).await?;
            Ok(addresses.take(65).collect())
        })
    }
}

pub(super) async fn resolve(
    resolver: &Arc<dyn DnsResolver>,
    url: &reqwest::Url,
    policy: &NetworkPolicy,
) -> Result<Vec<SocketAddr>, HttpError> {
    let host = url
        .host_str()
        .ok_or_else(|| HttpError::invalid("URL host"))?
        .trim_matches(['[', ']']);
    let port = url
        .port_or_known_default()
        .ok_or_else(|| HttpError::invalid("URL port"))?;
    let mut addresses = if let Ok(ip) = host.parse() {
        vec![SocketAddr::new(ip, port)]
    } else {
        resolver
            .resolve(host.into(), port)
            .await
            .map_err(|_| HttpError::invalid("DNS resolution").with_kind(HttpErrorKind::Dns))?
    };
    if addresses.is_empty() || addresses.len() > 64 {
        return Err(HttpError::invalid("DNS answer limits").with_kind(HttpErrorKind::Dns));
    }
    if addresses
        .iter()
        .any(|address| address.port() != port || !policy.permits(address.ip()))
    {
        return Err(HttpError::invalid("network address not authorized")
            .with_kind(HttpErrorKind::AddressDenied));
    }
    addresses.sort();
    addresses.dedup();
    Ok(addresses)
}

fn public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 192 && b == 0 && c == 0)
                && !(a == 192 && b == 88 && c == 99)
                && !(a == 198 && (b == 18 || b == 19))
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            // 只默认开放全球单播；隧道、文档与协议专用地址均需显式授予
            segments[0] & 0xe000 == 0x2000
                && !(segments[0] == 0x2001 && segments[1] < 0x0200)
                && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
                && segments[0] != 0x2002
                && !(segments[0] == 0x3fff && segments[1] < 0x1000)
        }
    }
}
