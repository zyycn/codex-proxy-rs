//! 验证固定目标地址的直连和代理连接保留 Host、SNI 与证书校验

use std::{sync::Arc, time::Duration};

use gateway_core::{account::OutboundProxy, upstream::UpstreamSendState};
use gateway_host::outbound::{HttpClient, HttpRequest, NetworkPolicy};

use crate::support::network::{FixedDns, Mode, ROOT, Server};

fn request(port: u16, secure: bool) -> HttpRequest {
    HttpRequest {
        method: "GET".into(),
        url: format!(
            "{}://upstream.test:{port}/query?value=1",
            if secure { "https" } else { "http" }
        ),
        headers: vec![],
        body: vec![],
    }
}

fn network() -> NetworkPolicy {
    NetworkPolicy::new(&["127.0.0.0/8".into(), "::1/128".into()]).unwrap()
}

#[tokio::test]
async fn pinned_connections_preserve_host_and_sni_for_direct_and_authenticated_proxies() {
    for proxy_kind in ["direct", "http", "https", "socks5", "socks5h"] {
        for secure in [false, true] {
            let mut upstream = Server::start(Mode::Origin, secure, "127.0.0.1:0").await;
            let mut proxy = match proxy_kind {
                "direct" => None,
                "http" | "https" => {
                    Some(Server::start(Mode::HttpProxy, proxy_kind == "https", "127.0.0.1:0").await)
                }
                _ => Some(Server::start(Mode::Socks, false, "127.0.0.1:0").await),
            };
            let route = proxy.as_ref().map(|proxy| {
                OutboundProxy::parse(&format!(
                    "{proxy_kind}://user:p%40ss@localhost:{}",
                    proxy.address.port()
                ))
                .unwrap()
            });
            let client = HttpClient::with_root_certificates(vec![ROOT.to_vec()])
                .unwrap()
                .with_resolver(Arc::new(FixedDns(upstream.address.ip())));
            let mut response = client
                .open(
                    request(upstream.address.port(), secure),
                    route.as_ref(),
                    &network(),
                    Duration::from_secs(3),
                )
                .await
                .unwrap_or_else(|error| panic!("{proxy_kind}, tls={secure}: {error:?}"));
            assert_eq!(response.status, 200);
            assert_eq!(response.body.read(64).await.unwrap().unwrap(), "secured");
            assert!(response.body.read(64).await.unwrap().is_none());
            let observation = upstream.next().await;
            assert!(
                observation
                    .head
                    .starts_with("GET /query?value=1 HTTP/1.1\r\n")
            );
            assert!(observation.head.to_ascii_lowercase().contains(&format!(
                "host: upstream.test:{}\r\n",
                upstream.address.port()
            )));
            assert!(
                !observation
                    .head
                    .to_ascii_lowercase()
                    .contains("proxy-authorization")
            );
            assert_eq!(
                observation.sni.as_deref(),
                secure.then_some("upstream.test")
            );
            if let Some(proxy) = &mut proxy {
                let observation = proxy.next().await;
                assert_eq!(observation.target, Some(upstream.address));
                if proxy_kind.starts_with("http") {
                    assert!(
                        observation
                            .head
                            .to_ascii_lowercase()
                            .contains("proxy-authorization: basic dxnlcjpwqhnz\r\n")
                    );
                    assert_eq!(
                        observation.sni.as_deref(),
                        (proxy_kind == "https").then_some("localhost")
                    );
                } else {
                    assert_eq!(
                        observation.authentication,
                        Some(("user".into(), "p@ss".into()))
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn pinning_never_disables_certificate_or_hostname_validation() {
    for trusted in [false, true] {
        let upstream = Server::start(Mode::Origin, true, "127.0.0.1:0").await;
        let client =
            HttpClient::with_root_certificates(if trusted { vec![ROOT.to_vec()] } else { vec![] })
                .unwrap()
                .with_resolver(Arc::new(FixedDns(upstream.address.ip())));
        let mut request = request(upstream.address.port(), true);
        if trusted {
            request.url = request.url.replace("upstream.test", "different.test");
        }
        let error = client
            .open(request, None, &network(), Duration::from_secs(3))
            .await
            .err()
            .unwrap();
        assert_eq!(error.send_state, UpstreamSendState::NotSent);
    }
}

#[tokio::test]
async fn socks_pins_ipv6_without_sending_a_bracketed_hostname() {
    let mut upstream = Server::start(Mode::Origin, false, "[::1]:0").await;
    let mut proxy = Server::start(Mode::Socks, false, "127.0.0.1:0").await;
    let route = OutboundProxy::parse(&format!("socks5h://user:pass@{}", proxy.address)).unwrap();
    let client = HttpClient::new()
        .unwrap()
        .with_resolver(Arc::new(FixedDns(upstream.address.ip())));
    let response = client
        .open(
            request(upstream.address.port(), false),
            Some(&route),
            &network(),
            Duration::from_secs(3),
        )
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(proxy.next().await.target, Some(upstream.address));
    assert!(upstream.next().await.head.starts_with("GET /query"));
}
