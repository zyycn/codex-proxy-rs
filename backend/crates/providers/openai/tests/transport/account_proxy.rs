use super::*;
use gateway_core::{
    account::{CredentialRevision, OutboundProxy, ProviderAccount, ProviderAccountId},
    routing::ProviderKind,
};

fn account(id: &str, proxy: Option<&str>) -> ProviderAccount {
    ProviderAccount::new(
        ProviderAccountId::new(format!("acct_{id}")).unwrap(),
        ProviderKind::new("openai").unwrap(),
        id.to_owned(),
        Some(id.to_owned()),
        "oauth".to_owned(),
        CredentialRevision::new(1).unwrap(),
        None,
    )
    .with_outbound_proxy(proxy.map(|value| OutboundProxy::parse(value).unwrap()))
}

async fn http_exit(label: &'static str) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request_with_body(&mut stream).await;
        let body = format!(
            "event: response.completed\ndata: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"resp_{label}\",\"status\":\"completed\",\"output\":[],\"usage\":{{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}}}}\n\n"
        );
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        let header_end = request
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap();
        String::from_utf8(request[..header_end].to_vec()).unwrap()
    });
    (format!("http://{address}"), task)
}

#[tokio::test]
async fn http_sse_accounts_use_distinct_proxies_and_clearing_restores_direct_client() {
    let (exit_a, a) = http_exit("exit_a").await;
    let (exit_b, b) = http_exit("exit_b").await;
    let (direct_url, direct) = http_exit("direct").await;
    let base = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        &direct_url,
        test_wire_profile(),
    );
    let client_a = base.for_account(&account("a", Some(&exit_a))).unwrap();
    let client_b = base.for_account(&account("b", Some(&exit_b))).unwrap();
    let mut request = codex_request("gpt-5.5", "", Vec::new());
    request.force_http_sse = true;
    let (response_a, response_b) = tokio::join!(
        client_a.create_response(&request, request_context("proxy-a", Some("a"))),
        client_b.create_response(&request, request_context("proxy-b", Some("b")))
    );
    assert!(response_a.unwrap().body.contains("resp_exit_a"));
    assert!(response_b.unwrap().body.contains("resp_exit_b"));
    let cleared = client_a.for_account(&account("a", None)).unwrap();
    assert!(
        cleared
            .create_response(&request, request_context("direct", Some("a")))
            .await
            .unwrap()
            .body
            .contains("resp_direct")
    );
    for head in [a.await.unwrap(), b.await.unwrap()] {
        assert!(head.starts_with(&format!("POST {direct_url}/codex/responses HTTP/1.1")));
    }
    assert!(
        direct
            .await
            .unwrap()
            .starts_with("POST /codex/responses HTTP/1.1")
    );
}

async fn websocket_exit(label: &'static str) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let connect = read_http_request(&mut stream).await;
        assert!(connect.starts_with("CONNECT upstream.invalid:80 HTTP/1.1"));
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        assert!(matches!(websocket.next().await, Some(Ok(Message::Text(_)))));
        websocket
            .send(Message::Text(
                completed_websocket_response(label, 2, 1).into(),
            ))
            .await
            .unwrap();
        connect
    });
    (format!("http://user:pass@{address}"), task)
}

#[tokio::test]
async fn websocket_connect_and_pool_are_isolated_when_account_proxy_changes() {
    let (exit_a, a) = websocket_exit("resp_exit_a").await;
    let (exit_b, b) = websocket_exit("resp_exit_b").await;
    let pool = Arc::new(CodexWebSocketPool::new(Duration::from_mins(1)));
    let base = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        "http://upstream.invalid",
        test_wire_profile(),
    )
    .with_websocket_pool(pool);
    let mut request = codex_request("gpt-5.5", "", Vec::new());
    request.set_previous_response_id(Some("resp_previous".to_owned()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);
    for (proxy, expected) in [(exit_a, "resp_exit_a"), (exit_b, "resp_exit_b")] {
        let client = base
            .for_account(&account("same-account", Some(&proxy)))
            .unwrap();
        let response = timeout(
            Duration::from_secs(5),
            client.create_response(&request, request_context("proxy-ws", Some("same-account"))),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(response.body.contains(expected));
    }
    for head in [a.await.unwrap(), b.await.unwrap()] {
        assert_eq!(
            read_header_value(&head, "Proxy-Authorization"),
            Some("Basic dXNlcjpwYXNz")
        );
    }
}

#[tokio::test]
async fn unavailable_proxy_fails_without_direct_fallback() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unreachable = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let direct = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{}", direct.local_addr().unwrap()),
        test_wire_profile(),
    );
    let client = base.for_account(&account("a", Some(&unreachable))).unwrap();
    let mut request = codex_request("gpt-5.5", "", Vec::new());
    request.force_http_sse = true;
    assert!(
        client
            .create_response(&request, request_context("blocked", Some("a")))
            .await
            .is_err()
    );
    assert!(
        timeout(Duration::from_millis(30), direct.accept())
            .await
            .is_err()
    );
}

async fn socks_exit(stream: &mut TcpStream) -> (u8, String, u16) {
    assert_eq!(stream.read_u8().await.unwrap(), 5);
    let count = stream.read_u8().await.unwrap();
    let mut methods = vec![0; usize::from(count)];
    stream.read_exact(&mut methods).await.unwrap();
    assert!(methods.contains(&2));
    stream.write_all(&[5, 2]).await.unwrap();
    assert_eq!(stream.read_u8().await.unwrap(), 1);
    let count = stream.read_u8().await.unwrap();
    let mut user = vec![0; usize::from(count)];
    stream.read_exact(&mut user).await.unwrap();
    let count = stream.read_u8().await.unwrap();
    let mut password = vec![0; usize::from(count)];
    stream.read_exact(&mut password).await.unwrap();
    assert_eq!(user, b"user@exit");
    assert_eq!(password, b"pass:word");
    stream.write_all(&[1, 0]).await.unwrap();
    let mut header = [0; 4];
    stream.read_exact(&mut header).await.unwrap();
    assert_eq!(&header[..3], &[5, 1, 0]);
    let host = match header[3] {
        1 => {
            let mut ip = [0; 4];
            stream.read_exact(&mut ip).await.unwrap();
            std::net::Ipv4Addr::from(ip).to_string()
        }
        4 => {
            let mut ip = [0; 16];
            stream.read_exact(&mut ip).await.unwrap();
            std::net::Ipv6Addr::from(ip).to_string()
        }
        3 => {
            let count = stream.read_u8().await.unwrap();
            let mut host = vec![0; usize::from(count)];
            stream.read_exact(&mut host).await.unwrap();
            String::from_utf8(host).unwrap()
        }
        _ => panic!("unexpected SOCKS address type"),
    };
    let port = stream.read_u16().await.unwrap();
    stream
        .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80])
        .await
        .unwrap();
    (header[3], host, port)
}

#[tokio::test]
async fn socks_account_egress_preserves_dns_mode_auth_and_ipv6_for_sse_and_websocket() {
    for websocket in [false, true] {
        let mut targets = vec![
            ("socks5", "localhost"),
            ("socks5h", "upstream.invalid"),
            ("socks5h", "[::1]"),
        ];
        // reqwest's local resolver does not accept IPv6 literals; use socks5h for those.
        if websocket {
            targets.push(("socks5", "[::1]"));
        }
        for (scheme, host) in targets {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy = format!(
                "{scheme}://user%40exit:pass%3Aword@{}",
                listener.local_addr().unwrap()
            );
            let task = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let target = socks_exit(&mut stream).await;
                if websocket {
                    let mut stream = accept_codex_test_websocket(stream).await;
                    assert!(matches!(stream.next().await, Some(Ok(Message::Text(_)))));
                    stream
                        .send(Message::Text(
                            completed_websocket_response("resp_socks", 2, 1).into(),
                        ))
                        .await
                        .unwrap();
                } else {
                    read_http_request_with_body(&mut stream).await;
                    let body = format!(
                        "data: {}\n\n",
                        completed_websocket_response("resp_socks", 2, 1)
                    );
                    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                }
                target
            });
            let client = CodexBackendClient::new(
                reqwest::Client::builder().no_proxy().build().unwrap(),
                format!("http://{host}"),
                test_wire_profile(),
            )
            .for_account(&account("socks", Some(&proxy)))
            .unwrap();
            let mut request = codex_request("gpt-5.5", "", Vec::new());
            request.force_http_sse = !websocket;
            if websocket {
                request.set_previous_response_id(Some("resp_previous".to_owned()));
                request.previous_response_scope = Some(PreviousResponseScope::Persisted);
            }
            let response = timeout(
                Duration::from_secs(5),
                client.create_response(&request, request_context("socks", Some("socks"))),
            )
            .await
            .unwrap()
            .unwrap_or_else(|error| {
                panic!("websocket={websocket}, scheme={scheme}, host={host}: {error}")
            });
            assert!(response.body.contains("resp_socks"));
            let (address_type, target, port) = task.await.unwrap();
            assert_eq!(port, 80);
            match host {
                "localhost" => {
                    assert!(matches!(address_type, 1 | 4));
                    assert!(target.parse::<std::net::IpAddr>().unwrap().is_loopback());
                }
                "[::1]" => assert_eq!((address_type, target.as_str()), (4, "::1")),
                _ => assert_eq!((address_type, target.as_str()), (3, host)),
            }
        }
    }
}

#[tokio::test]
async fn https_account_proxy_starts_tls_and_rejection_never_falls_back_to_direct() {
    for websocket in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = format!("https://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut header = [0; 5];
            stream.read_exact(&mut header).await.unwrap();
            assert_eq!(&header[..2], &[22, 3]);
            let mut hello = vec![0; usize::from(u16::from_be_bytes([header[3], header[4]]))];
            stream.read_exact(&mut hello).await.unwrap();
            stream.write_all(&[21, 3, 3, 0, 2, 2, 40]).await.unwrap();
        });
        let direct = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = CodexBackendClient::new(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            format!("http://{}", direct.local_addr().unwrap()),
            test_wire_profile(),
        )
        .for_account(&account("tls-proxy", Some(&proxy)))
        .unwrap();
        let mut request = codex_request("gpt-5.5", "", Vec::new());
        request.force_http_sse = !websocket;
        if websocket {
            request.set_previous_response_id(Some("resp_previous".to_owned()));
            request.previous_response_scope = Some(PreviousResponseScope::Persisted);
        }
        assert!(
            timeout(
                Duration::from_secs(5),
                client.create_response(&request, request_context("tls-proxy", Some("tls-proxy")))
            )
            .await
            .unwrap()
            .is_err()
        );
        timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(
            timeout(Duration::from_millis(30), direct.accept())
                .await
                .is_err()
        );
    }
}
