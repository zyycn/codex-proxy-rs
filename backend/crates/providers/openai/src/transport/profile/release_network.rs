//! 官方发布检测共用的 HTTP 客户端，出口由 Provider 启动配置固定

use std::time::Duration;

use gateway_core::account::OutboundProxy;
use reqwest::{Client, Proxy, redirect::Policy};

pub(crate) fn build_release_client(
    proxy: Option<&OutboundProxy>,
) -> Result<Client, reqwest::Error> {
    let mut builder = Client::builder()
        .no_proxy()
        .https_only(true)
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30));
    if let Some(proxy) = proxy {
        builder = builder.proxy(Proxy::all(proxy.expose_url())?);
    }
    builder.build()
}
