//! 通过生产辅助请求 transport 观察出口 cache 的真实连接生命周期。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use gateway_core::account::OutboundProxy;
use provider_xai::{
    GrokBillingClient, GrokModelCatalogSession, ReqwestGrokInferenceTransport,
    ReqwestGrokModelCatalogTransport, SecretValue,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::{JoinHandle, JoinSet};
use url::Url;

use crate::support::{loopback_endpoint_policy, xai_wire_profile};

#[tokio::test]
async fn auxiliary_pool_reuses_connections_and_evicts_the_least_recent_exit() {
    let exit = KeepAliveExit::start().await;
    let transport = Arc::new(
        ReqwestGrokModelCatalogTransport::new(loopback_endpoint_policy(&exit.origin)).unwrap(),
    );
    let client = GrokBillingClient::new(transport);
    let capacity = ReqwestGrokInferenceTransport::MAX_CACHED_ACCOUNT_CLIENTS;
    let proxies = (0..=capacity)
        .map(|index| {
            let mut url = exit.origin.clone();
            url.set_username(&format!("synthetic-exit-{index}"))
                .unwrap();
            OutboundProxy::parse(url.as_str()).unwrap()
        })
        .collect::<Vec<_>>();
    for proxy in &proxies {
        fetch_billing(&client, proxy).await;
    }
    assert_eq!(exit.connections.load(Ordering::SeqCst), capacity + 1);
    fetch_billing(&client, proxies.last().unwrap()).await;
    assert_eq!(
        exit.connections.load(Ordering::SeqCst),
        capacity + 1,
        "命中出口应复用实际 TCP 连接，而不只是复用构造计数"
    );
    fetch_billing(&client, &proxies[0]).await;
    assert_eq!(
        exit.connections.load(Ordering::SeqCst),
        capacity + 2,
        "最旧出口已淘汰，重新请求必须建立新连接"
    );
}

async fn fetch_billing(client: &GrokBillingClient, proxy: &OutboundProxy) {
    let session = GrokModelCatalogSession::new(
        SecretValue::new("synthetic-access"),
        SecretValue::new("synthetic-user"),
        None,
        xai_wire_profile(),
    )
    .unwrap()
    .with_outbound_proxy(Some(proxy.clone()));
    tokio::time::timeout(Duration::from_secs(5), client.fetch(&session))
        .await
        .unwrap()
        .unwrap();
}

struct KeepAliveExit {
    origin: Url,
    connections: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl KeepAliveExit {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let connections = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::clone(&connections);
        let task = tokio::spawn(async move {
            let mut requests = JoinSet::new();
            loop {
                tokio::select! {
                    connection = listener.accept() => {
                        let (mut socket, _) = connection.unwrap();
                        accepted.fetch_add(1, Ordering::SeqCst);
                        requests.spawn(async move {
                            loop {
                                let mut header = Vec::new();
                                while !header.ends_with(b"\r\n\r\n") {
                                    let Ok(byte) = socket.read_u8().await else { return; };
                                    header.push(byte);
                                    assert!(header.len() < 16 * 1024);
                                }
                                assert!(header.starts_with(b"GET "));
                                let body = r#"{"config":{"creditUsagePercent":25}}"#;
                                let response = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                                    body.len(),
                                );
                                if socket.write_all(response.as_bytes()).await.is_err() {
                                    return;
                                }
                            }
                        });
                    }
                    result = requests.join_next(), if !requests.is_empty() => {
                        result.unwrap().unwrap();
                    }
                }
            }
        });
        Self {
            origin,
            connections,
            task,
        }
    }
}

impl Drop for KeepAliveExit {
    fn drop(&mut self) {
        self.task.abort();
    }
}
