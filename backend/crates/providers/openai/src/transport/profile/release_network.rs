//! 官方发布检测的 HTTP 出口，每个检测任务独立持有连接快照

use std::sync::{Arc, RwLock};
use std::time::Duration;

use gateway_core::account::OutboundProxy;
use reqwest::{Client, Proxy, redirect::Policy};

#[derive(Clone)]
pub struct ReleaseNetwork(Arc<RwLock<Client>>);

impl ReleaseNetwork {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self(Arc::new(RwLock::new(Self::build(None)?))))
    }

    pub fn configure(&self, proxy: Option<&OutboundProxy>) -> Result<(), reqwest::Error> {
        let client = Self::build(proxy)?;
        *self.0.write().expect("release network lock poisoned") = client;
        Ok(())
    }

    pub fn client(&self) -> Client {
        self.0
            .read()
            .expect("release network lock poisoned")
            .clone()
    }

    fn build(proxy: Option<&OutboundProxy>) -> Result<Client, reqwest::Error> {
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
}
