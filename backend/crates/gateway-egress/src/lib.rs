//! 网关 HTTP 与 WebSocket 出口：业务指定的代理优先，否则按目的地址使用部署环境

use std::sync::OnceLock;

use gateway_core::account::{InvalidOutboundProxy, OutboundProxy};
use hyper_util::client::proxy::matcher::Matcher;
use reqwest::{Client, ClientBuilder, NoProxy, Proxy};
use url::Url;

struct Environment {
    http: Option<OutboundProxy>,
    https: Option<OutboundProxy>,
    no_proxy: String,
    matcher: Matcher,
}

impl Environment {
    fn load() -> Result<Self, InvalidOutboundProxy> {
        let var = |name: &str| {
            std::env::var(name)
                .or_else(|_| std::env::var(name.to_ascii_lowercase()))
                .ok()
                .filter(|value| !value.is_empty())
        };
        // CGI 的 HTTP_* 来自入站请求头，不能作为部署配置
        let no_proxy = var("NO_PROXY").unwrap_or_default();
        let bypass_all = std::env::var_os("REQUEST_METHOD").is_some()
            || no_proxy.split(',').any(|entry| entry.trim() == "*");
        let all = var("ALL_PROXY");
        let route = |name| {
            if bypass_all {
                return Ok(None);
            }
            var(name)
                .or_else(|| all.clone())
                .map(|value| {
                    // 部署环境也接受常见的 host:port 写法，服务内保存的代理仍须是完整 URL
                    if value.contains("://") {
                        OutboundProxy::parse(&value)
                    } else {
                        OutboundProxy::parse(&format!("http://{value}"))
                    }
                })
                .transpose()
        };
        let http = route("HTTP_PROXY")?;
        let https = route("HTTPS_PROXY")?;
        let matcher = Matcher::builder()
            .http(http.as_ref().map_or("", OutboundProxy::expose_url))
            .https(https.as_ref().map_or("", OutboundProxy::expose_url))
            .no(no_proxy.as_str())
            .build();
        Ok(Self {
            http,
            https,
            no_proxy,
            matcher,
        })
    }
}

fn environment() -> Result<&'static Environment, InvalidOutboundProxy> {
    static ENV: OnceLock<Result<Environment, InvalidOutboundProxy>> = OnceLock::new();
    ENV.get_or_init(Environment::load).as_ref().map_err(|e| *e)
}

/// 每个请求（包括重定向）按目标匹配环境代理；显式代理不受 NO_PROXY 影响
pub fn http_client(proxy: Option<&OutboundProxy>) -> Result<ClientBuilder, InvalidOutboundProxy> {
    let mut builder = Client::builder().no_proxy();
    if let Some(proxy) = proxy {
        return Ok(builder.proxy(Proxy::all(proxy.expose_url()).map_err(|_| InvalidOutboundProxy)?));
    }
    let env = environment()?;
    let bypass = NoProxy::from_string(&env.no_proxy);
    if let Some(proxy) = &env.http {
        builder = builder.proxy(
            Proxy::http(proxy.expose_url())
                .map_err(|_| InvalidOutboundProxy)?
                .no_proxy(bypass.clone()),
        );
    }
    if let Some(proxy) = &env.https {
        builder = builder.proxy(
            Proxy::https(proxy.expose_url())
                .map_err(|_| InvalidOutboundProxy)?
                .no_proxy(bypass),
        );
    }
    Ok(builder)
}

/// 自行拨号的传输复用 reqwest 使用的匹配器，但保留原代理的凭据与连接限制
pub fn for_destination<'a>(
    proxy: Option<&'a OutboundProxy>,
    url: &Url,
) -> Result<Option<&'a OutboundProxy>, InvalidOutboundProxy> {
    if proxy.is_some() {
        return Ok(proxy);
    }
    let env = environment()?;
    let (scheme, proxy) = match url.scheme() {
        "http" | "ws" => ("http", env.http.as_ref()),
        "https" | "wss" => ("https", env.https.as_ref()),
        _ => return Err(InvalidOutboundProxy),
    };
    let Some(proxy) = proxy else {
        return Ok(None);
    };
    let authority = &url[url::Position::BeforeHost..url::Position::AfterPort];
    let uri = http::Uri::builder()
        .scheme(scheme)
        .authority(authority)
        .path_and_query("/")
        .build()
        .map_err(|_| InvalidOutboundProxy)?;
    Ok(env.matcher.intercept(&uri).map(|_| proxy))
}
