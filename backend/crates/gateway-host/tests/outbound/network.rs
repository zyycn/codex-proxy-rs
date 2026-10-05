//! 验证 DNS 固定、地址范围校验与解析超时的发送状态

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::future::BoxFuture;
use gateway_core::upstream::UpstreamSendState;
use gateway_host::outbound::{DnsResolver, HttpClient, HttpErrorKind, HttpRequest, NetworkPolicy};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

use crate::support::network::FixedDns;

#[test]
fn private_special_and_mapped_addresses_require_explicit_ranges() {
    let policy = NetworkPolicy::default();
    for ip in [
        "0.0.0.0",
        "10.0.0.1",
        "100.64.0.1",
        "127.0.0.1",
        "169.254.169.254",
        "172.16.0.1",
        "192.168.0.1",
        "192.0.2.1",
        "198.18.0.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "240.0.0.1",
        "255.255.255.255",
        "::",
        "::1",
        "::ffff:127.0.0.1",
        "fc00::1",
        "fe80::1",
        "ff02::1",
        "64:ff9b::7f00:1",
        "2001:db8::1",
        "2002:7f00:1::",
        "3fff::1",
    ] {
        assert!(
            !policy.permits(ip.parse().unwrap()),
            "unexpected public address: {ip}"
        );
    }
    for ip in [
        "1.1.1.1",
        "8.8.8.8",
        "2606:4700:4700::1111",
        "2001:4860:4860::8888",
    ] {
        assert!(policy.permits(ip.parse().unwrap()));
    }
    let granted = NetworkPolicy::new(&["127.0.0.1/32".into(), "fd12:3456::/32".into()]).unwrap();
    assert!(granted.permits("::ffff:127.0.0.1".parse().unwrap()));
    assert!(granted.permits("fd12:3456::1234".parse().unwrap()));
    assert!(!granted.permits("127.0.0.2".parse().unwrap()));
    assert!(NetworkPolicy::new(&["*".into()]).is_err());
}

struct ChangingDns {
    calls: Arc<AtomicUsize>,
    mixed: bool,
}

impl DnsResolver for ChangingDns {
    fn resolve(
        &self,
        _: String,
        port: u16,
    ) -> BoxFuture<'static, std::io::Result<Vec<SocketAddr>>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let addresses = if self.mixed {
            vec![
                SocketAddr::from(([127, 0, 0, 1], port)),
                SocketAddr::from(([127, 0, 0, 2], port)),
            ]
        } else {
            vec![SocketAddr::from((
                [127, 0, 0, if call == 0 { 1 } else { 2 }],
                port,
            ))]
        };
        Box::pin(async move { Ok(addresses) })
    }
}

fn request(url: String) -> HttpRequest {
    HttpRequest {
        method: "GET".into(),
        url,
        headers: vec![],
        body: vec![],
    }
}

#[tokio::test]
async fn dns_is_pinned_and_revalidated_even_when_a_pooled_connection_exists() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string("authorized"))
        .expect(1)
        .mount(&server)
        .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let client = HttpClient::new()
        .unwrap()
        .with_resolver(Arc::new(ChangingDns {
            calls: calls.clone(),
            mixed: false,
        }));
    let network = NetworkPolicy::new(&["127.0.0.1/32".into()]).unwrap();
    let url = format!("http://dns-rebinding.invalid:{}/", server.address().port());
    let mut response = client
        .open(request(url.clone()), None, &network, Duration::from_secs(3))
        .await
        .unwrap();
    assert_eq!(
        response.body.read(1024).await.unwrap().unwrap(),
        "authorized"
    );
    assert!(response.body.read(1024).await.unwrap().is_none());
    drop(response);
    let error = client
        .open(request(url), None, &network, Duration::from_secs(3))
        .await
        .err()
        .unwrap();
    assert_eq!(error.send_state, UpstreamSendState::NotSent);
    assert_eq!(error.kind(), HttpErrorKind::AddressDenied);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn mixed_dns_answers_and_implicit_loopback_access_are_rejected_before_http() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let client = HttpClient::new()
        .unwrap()
        .with_resolver(Arc::new(ChangingDns { calls, mixed: true }));
    let network = NetworkPolicy::new(&["127.0.0.1/32".into()]).unwrap();
    let error = client
        .open(
            request(format!("http://mixed.invalid:{}/", server.address().port())),
            None,
            &network,
            Duration::from_secs(2),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.send_state, UpstreamSendState::NotSent);
    let error = client
        .open(
            request(server.uri()),
            None,
            &NetworkPolicy::default(),
            Duration::from_secs(2),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.send_state, UpstreamSendState::NotSent);
}

struct SlowDns;
impl DnsResolver for SlowDns {
    fn resolve(&self, _: String, _: u16) -> BoxFuture<'static, std::io::Result<Vec<SocketAddr>>> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(vec![])
        })
    }
}

#[tokio::test]
async fn dns_consumes_the_same_deadline_and_timeout_is_not_sent() {
    let client = HttpClient::new().unwrap().with_resolver(Arc::new(SlowDns));
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        client.open(
            request("https://slow.invalid/".into()),
            None,
            &NetworkPolicy::default(),
            Duration::from_millis(20),
        ),
    )
    .await
    .unwrap()
    .err()
    .unwrap();
    assert_eq!(error.send_state, UpstreamSendState::NotSent);
    assert_eq!(error.kind(), HttpErrorKind::Timeout);
}

#[tokio::test]
async fn fake_ip_and_private_dns_answers_stay_denied_without_leaking_the_url() {
    for ip in ["198.18.0.23", "127.0.0.1", "169.254.169.254", "10.0.0.1"] {
        let client = HttpClient::new()
            .unwrap()
            .with_resolver(Arc::new(FixedDns(ip.parse().unwrap())));
        let error = client
            .open(
                request("https://example.invalid/private?token=do-not-log".into()),
                None,
                &NetworkPolicy::default(),
                Duration::from_secs(1),
            )
            .await
            .err()
            .unwrap();
        assert_eq!(error.kind(), HttpErrorKind::AddressDenied);
        assert_eq!(error.send_state, UpstreamSendState::NotSent);
        assert_eq!(error.reason(), "network address not authorized");
        assert!(!format!("{error:?} {error}").contains("do-not-log"));
    }
}

struct FailingDns;

impl DnsResolver for FailingDns {
    fn resolve(&self, _: String, _: u16) -> BoxFuture<'static, std::io::Result<Vec<SocketAddr>>> {
        Box::pin(async { Err(std::io::Error::other("resolver details must not escape")) })
    }
}

#[tokio::test]
async fn dns_failures_are_distinct_from_address_denials_and_transport_errors() {
    let client = HttpClient::new()
        .unwrap()
        .with_resolver(Arc::new(FailingDns));
    let error = client
        .open(
            request("https://example.invalid/".into()),
            None,
            &NetworkPolicy::default(),
            Duration::from_secs(1),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), HttpErrorKind::Dns);
    assert_eq!(error.send_state, UpstreamSendState::NotSent);
    assert!(!format!("{error:?} {error}").contains("resolver details"));
}
