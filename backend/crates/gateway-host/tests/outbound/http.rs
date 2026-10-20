//! 验证受管 HTTP 的协议、网络约束、时限与正文资源回收

use std::time::Duration;

use gateway_core::{account::OutboundProxy, upstream::UpstreamSendState};
use gateway_host::outbound::{HttpClient, HttpErrorKind, HttpRequest, NetworkPolicy};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

fn request(url: String) -> HttpRequest {
    HttpRequest {
        method: "GET".into(),
        url,
        headers: vec![],
        body: vec![],
        maximum_body_bytes: 0,
    }
}

fn local_network() -> NetworkPolicy {
    NetworkPolicy::new(&["127.0.0.0/8".into(), "::1/128".into()]).unwrap()
}

#[tokio::test]
async fn managed_https_http2_uses_authority_without_an_extra_host_header() {
    use std::sync::Arc;

    use crate::support::network::{FixedDns, Mode, ROOT, Server};

    let mut server = Server::start(Mode::Http2Origin, true, "127.0.0.1:0").await;
    let url = format!("https://upstream.test:{}/query", server.address.port());
    let client = HttpClient::with_root_certificates(vec![ROOT.to_vec()])
        .unwrap()
        .with_resolver(Arc::new(FixedDns(server.address.ip())));
    let mut response = client
        .open(
            request(url.clone()),
            None,
            &local_network(),
            Duration::from_secs(3),
        )
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body.read(64).await.unwrap().unwrap(), "secured");
    let observation = server.next().await;
    assert!(
        observation
            .head
            .starts_with(&format!("GET {url} HTTP/2.0\r\n"))
    );
    assert_eq!(observation.sni.as_deref(), Some("upstream.test"));
}

#[tokio::test]
async fn managed_http_does_not_follow_redirects_or_fall_back_from_proxy() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "http://unresolvable.invalid/"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = HttpClient::new().unwrap();
    let response = client
        .open(
            request(server.uri()),
            None,
            &local_network(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
    assert_eq!(response.status, 302);
    drop(response);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy =
        OutboundProxy::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    drop(listener);
    let error = client
        .open(
            request(server.uri()),
            Some(&proxy),
            &local_network(),
            Duration::from_secs(2),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.send_state, UpstreamSendState::NotSent);
    assert_eq!(error.kind(), HttpErrorKind::Transport);
}

#[tokio::test]
async fn managed_http_caps_chunks_and_recovers_capacity_after_body_drop() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![42; 300_000]))
        .mount(&server)
        .await;
    let client = HttpClient::new().unwrap();
    let mut response = client
        .open(
            request(server.uri()),
            None,
            &local_network(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    let mut size = 0;
    while let Some(chunk) = response.body.read(1003).await.unwrap() {
        assert!(chunk.len() <= 1003);
        assert!(chunk.iter().all(|byte| *byte == 42));
        size += chunk.len();
    }
    assert_eq!(size, 300_000);
    drop(response);
    let mut bodies = Vec::new();
    for _ in 0..128 {
        bodies.push(
            client
                .open(
                    request(server.uri()),
                    None,
                    &local_network(),
                    Duration::from_secs(5),
                )
                .await
                .unwrap(),
        );
    }
    assert_eq!(
        client
            .open(
                request(server.uri()),
                None,
                &local_network(),
                Duration::from_secs(5)
            )
            .await
            .err()
            .unwrap()
            .send_state,
        UpstreamSendState::NotSent
    );
    bodies.pop();
    assert!(
        client
            .open(
                request(server.uri()),
                None,
                &local_network(),
                Duration::from_secs(5)
            )
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn managed_http_deadline_and_invalid_headers_preserve_send_facts() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(300)))
        .expect(1)
        .mount(&server)
        .await;
    let client = HttpClient::new().unwrap();
    let mut invalid = request(server.uri());
    invalid
        .headers
        .push(("proxy-authorization".into(), b"secret-test-only".to_vec()));
    let error = client
        .open(invalid, None, &local_network(), Duration::from_secs(2))
        .await
        .err()
        .unwrap();
    assert_eq!(error.send_state, UpstreamSendState::NotSent);
    assert_eq!(error.kind(), HttpErrorKind::InvalidRequest);
    assert!(!format!("{error:?}").contains("secret-test-only"));
    let error = client
        .open(
            request(server.uri()),
            None,
            &local_network(),
            Duration::from_millis(100),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.send_state, UpstreamSendState::Ambiguous);
    assert_eq!(error.kind(), HttpErrorKind::Timeout);
}

#[tokio::test]
async fn buffered_body_cannot_extend_the_request_deadline() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![42; 1024]))
        .expect(1)
        .mount(&server)
        .await;
    let client = HttpClient::new().unwrap();
    let mut response = client
        .open(
            request(server.uri()),
            None,
            &local_network(),
            Duration::from_millis(300),
        )
        .await
        .unwrap();
    assert_eq!(
        response.body.read(1).await.unwrap().unwrap().as_ref(),
        &[42]
    );
    // 上游已返回整块数据，插件延迟读取也不能绕过调用期限
    tokio::time::sleep(Duration::from_millis(320)).await;
    let error = response.body.read(1).await.unwrap_err();
    assert_eq!(error.send_state, UpstreamSendState::Sent);
    assert_eq!(error.kind(), HttpErrorKind::Timeout);
}

#[tokio::test]
async fn resource_body_reads_outlive_opening_but_keep_a_per_read_timeout() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (send_next, next) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        let mut buffer = [0; 1024];
        while !head.ends_with(b"\r\n\r\n") {
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0 && head.len() < 16 * 1024);
            head.extend_from_slice(&buffer[..count]);
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\na")
            .await
            .unwrap();
        next.await.unwrap();
        socket.write_all(b"b").await.unwrap();
        assert_eq!(socket.read(&mut buffer).await.unwrap(), 0);
    });
    let mut response = HttpClient::new()
        .unwrap()
        .open(request(url), None, &local_network(), Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(response.body.read(1).await.unwrap().unwrap(), "a");
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::time::resume();
    send_next.send(()).unwrap();
    assert_eq!(
        response
            .body
            .read_with_timeout(1, Duration::from_secs(1))
            .await
            .unwrap()
            .unwrap(),
        "b"
    );
    let error = response
        .body
        .read_with_timeout(1, Duration::from_millis(100))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), HttpErrorKind::Timeout);
    assert_eq!(error.send_state, UpstreamSendState::Sent);
    drop(response);
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn cancelling_dns_releases_the_request_slot_without_starting_http() {
    use std::sync::Arc;

    use futures::future::BoxFuture;
    use gateway_host::outbound::DnsResolver;

    struct PendingDns;
    impl DnsResolver for PendingDns {
        fn resolve(
            &self,
            _: String,
            _: u16,
        ) -> BoxFuture<'static, std::io::Result<Vec<std::net::SocketAddr>>> {
            Box::pin(std::future::pending())
        }
    }

    let client = HttpClient::new()
        .unwrap()
        .with_resolver(Arc::new(PendingDns));
    // 超过总并发容量，逐次取消仍不能积累占用
    for _ in 0..129 {
        assert!(
            tokio::time::timeout(
                Duration::from_millis(1),
                client.open(
                    request("https://pending.invalid/".into()),
                    None,
                    &NetworkPolicy::default(),
                    Duration::from_secs(1),
                ),
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test]
async fn managed_http_enforces_the_callers_body_limit_before_sending() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&server)
        .await;
    let client = HttpClient::new().unwrap();
    // 覆盖旧固定上限之外的正文，并验证同一个客户端逐次使用调用方的预算
    let maximum = 17 * 1024 * 1024;
    for (size, limit, accepted) in [
        (maximum, maximum, true),
        (maximum + 1, maximum, false),
        (maximum + 1, maximum + 1, true),
    ] {
        let mut request = request(server.uri());
        request.method = "POST".into();
        request.body = vec![b'x'; size];
        request.maximum_body_bytes = limit;
        let result = client
            .open(request, None, &local_network(), Duration::from_secs(10))
            .await;
        if accepted {
            assert_eq!(result.unwrap().status, 200);
        } else {
            let error = result.err().unwrap();
            assert_eq!(error.kind(), HttpErrorKind::InvalidRequest);
            assert_eq!(error.send_state, UpstreamSendState::NotSent);
        }
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body.len(), maximum);
    assert_eq!(requests[1].body.len(), maximum + 1);
}
