//! 验证网关特化恢复与普通上游错误透传的边界

use super::precommit::stream_failure;
use super::*;

#[tokio::test]
async fn ordinary_http_and_websocket_opening_errors_remain_client_decisions() {
    for websocket in [false, true] {
        for status in [409, 500, 503, 504] {
            let store = Arc::new(MemoryAccountStore::default());
            create_account(&store, "acct_provider_contract").await;
            let server = MockServer::start().await;
            let body = r#"{ "error":{"code":"Vendor.Future.v2","type":"server_error","message":"upstream failed","future":9007199254740993} }"#;
            Mock::given(method(if websocket { "GET" } else { "POST" }))
                .and(path("/codex/responses"))
                .respond_with(
                    ResponseTemplate::new(status)
                        .insert_header("content-type", "application/json")
                        .insert_header("retry-after", "17")
                        .set_body_string(body),
                )
                .expect(2)
                .mount(&server)
                .await;
            let provider = provider_with_base_url_and_retry_budget(&store, server.uri(), 0);
            // 同一会话的下一次请求仍使用原传输，普通拒绝不污染 WS 回退状态
            for _ in 0..2 {
                let operation = if websocket {
                    capacity_websocket_operation()
                } else {
                    http_generate_operation()
                };
                let mut stream = provider
                    .clone()
                    .execute(
                        planned_request("openai", operation),
                        context("req_passthrough_recovery", CancellationToken::new()),
                    )
                    .await
                    .unwrap();
                let (_, error) = stream_failure(&mut stream).await;
                assert!(
                    error.retry_is_prohibited(),
                    "websocket={websocket}, status={status}"
                );
                assert!(!error.replay_is_safe());
                assert!(error.pre_delivery_retry().is_none());
                let response = error.client_visible_upstream_response().unwrap();
                assert_eq!(response.status(), status);
                assert_eq!(response.body().as_ref(), body.as_bytes());
                assert!(response.headers().iter().any(
                    |header| header.name() == "retry-after" && header.value().as_ref() == b"17"
                ));
            }
            server.verify().await;
        }
    }
}

#[tokio::test]
async fn ordinary_stream_failures_preserve_original_events_without_proxy_retry() {
    for websocket in [false, true] {
        for code in ["server_error", "Vendor.Future.v2"] {
            let store = Arc::new(MemoryAccountStore::default());
            create_account(&store, "acct_provider_contract").await;
            let created = json!({"type":"response.created","response":{"id":"resp_failure","status":"in_progress","output":[]}});
            let failed = json!({"type":"response.failed","response":{"id":"resp_failure","status":"failed","output":[],"error":{"code":code,"type":"server_error","message":"original failure","headers":{"Retry-After":"17"},"future":{"keep":true}}}});
            let expected = vec![created.clone(), failed.clone()];
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                if websocket {
                    let mut socket = accept_codex_test_websocket(socket).await;
                    socket.next().await.unwrap().unwrap();
                    for event in [created, failed] {
                        socket
                            .send(Message::Text(event.to_string().into()))
                            .await
                            .unwrap();
                    }
                } else {
                    read_http_request(&mut socket).await;
                    let body = [created, failed]
                        .iter()
                        .map(|event| {
                            gateway_protocol::openai::sse::encode_sse_event(
                                event["type"].as_str().unwrap(),
                                &event.to_string(),
                            )
                        })
                        .collect::<String>();
                    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                }
            });
            let operation = if websocket {
                capacity_websocket_operation()
            } else {
                http_generate_operation()
            };
            let mut stream = provider_with_base_url(&store, base)
                .execute(
                    planned_request("openai", operation),
                    context("req_stream_passthrough", CancellationToken::new()),
                )
                .await
                .unwrap();
            let (events, mut error) = stream_failure(&mut stream).await;
            assert!(events.iter().all(|event| !event.has_client_event()));
            assert!(error.retry_is_prohibited());
            assert!(error.pre_delivery_retry().is_none());
            assert_eq!(error.retry_after(), Some(Duration::from_secs(17)));
            let atomic = error.take_atomic_client_events();
            assert_eq!(
                atomic
                    .iter()
                    .filter_map(|event| event.wire_event().map(|wire| wire.data().clone()))
                    .collect::<Vec<_>>(),
                expected
            );
            server.await.unwrap();
        }
    }
}

#[cfg(target_os = "linux")]
mod native_tls {
    use super::*;

    fn run_isolated_default_tls() -> bool {
        const CHILD: &str = "CPR_TEST_NATIVE_TLS_RECOVERY";
        if std::env::var_os(CHILD).is_some() {
            return false;
        }
        // Cargo 的 SSL_CERT_FILE 会选择 rustls；只在子进程中固定默认 native TLS 路径
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", std::thread::current().name().unwrap()])
            .env(CHILD, "1")
            .env_remove(provider_openai::transport::tls::CODEX_CA_CERT_ENV)
            .env_remove(provider_openai::transport::tls::SSL_CERT_FILE_ENV)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        true
    }

    async fn failure_at(base: String) -> gateway_core::error::ProviderError {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let mut stream = provider_with_base_url(&store, base)
            .execute(
                planned_request("openai", http_generate_operation()),
                context("req_tls_recovery", CancellationToken::new()),
            )
            .await
            .unwrap();
        let (_, error) = timeout(Duration::from_secs(5), stream_failure(&mut stream))
            .await
            .unwrap();
        assert_eq!(error.send_state(), UpstreamSendState::NotSent);
        error
    }

    async fn rejected_handshake(alert: bool) -> gateway_core::error::ProviderError {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!(
            "https://localhost:{}",
            listener.local_addr().unwrap().port()
        );
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut header = [0_u8; 5];
            socket.read_exact(&mut header).await.unwrap();
            assert_eq!(header[0], 22);
            let mut hello = vec![0; usize::from(u16::from_be_bytes([header[3], header[4]]))];
            socket.read_exact(&mut hello).await.unwrap();
            if alert {
                socket.write_all(&[21, 3, 3, 0, 2, 2, 40]).await.unwrap();
            }
            socket.shutdown().await.unwrap();
        });
        let error = failure_at(base).await;
        server.await.unwrap();
        error
    }

    #[tokio::test]
    async fn handshake_eof_requests_bounded_same_account_connection_recovery() {
        if run_isolated_default_tls() {
            return;
        }
        let error = rejected_handshake(false).await;
        assert!(error.error_details().unwrap().contains("unexpected EOF"));
        assert_eq!(
            error.pre_delivery_retry(),
            Some(PreDeliveryRetry::SameAccountConnectionRetry {
                transport: AttemptTransport::Fallback
            })
        );
    }

    #[tokio::test]
    async fn handshake_alert_does_not_request_connection_recovery() {
        if run_isolated_default_tls() {
            return;
        }
        let error = rejected_handshake(true).await;
        assert!(error.error_details().unwrap().contains("handshake failure"));
        assert!(error.pre_delivery_retry().is_none());
    }

    #[tokio::test]
    async fn untrusted_certificate_does_not_request_connection_recovery() {
        if run_isolated_default_tls() {
            return;
        }
        use openssl::{
            asn1::Asn1Time,
            hash::MessageDigest,
            pkey::PKey,
            rsa::Rsa,
            ssl::{SslAcceptor, SslMethod},
            x509::{X509, X509NameBuilder},
        };
        let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_text("CN", "localhost").unwrap();
        let name = name.build();
        let mut certificate = X509::builder().unwrap();
        certificate.set_version(2).unwrap();
        certificate.set_subject_name(&name).unwrap();
        certificate.set_issuer_name(&name).unwrap();
        certificate.set_pubkey(&key).unwrap();
        certificate
            .set_not_before(&Asn1Time::days_from_now(0).unwrap())
            .unwrap();
        certificate
            .set_not_after(&Asn1Time::days_from_now(1).unwrap())
            .unwrap();
        certificate.sign(&key, MessageDigest::sha256()).unwrap();
        let mut acceptor = SslAcceptor::mozilla_intermediate(SslMethod::tls()).unwrap();
        acceptor.set_private_key(&key).unwrap();
        acceptor.set_certificate(&certificate.build()).unwrap();
        let acceptor = acceptor.build();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!(
            "https://localhost:{}",
            listener.local_addr().unwrap().port()
        );
        let server = tokio::task::spawn_blocking(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            assert!(acceptor.accept(socket).is_err());
        });
        let error = failure_at(base).await;
        assert!(
            error
                .error_details()
                .unwrap()
                .contains("certificate verify failed")
        );
        assert!(error.pre_delivery_retry().is_none());
        server.await.unwrap();
    }
}
