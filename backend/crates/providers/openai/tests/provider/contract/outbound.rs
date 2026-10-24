//! 最终出站准备覆盖真实字节、目标以及原生连接归属

use super::*;
use crate::transport::accept_codex_test_websocket_with;
use gateway_core::engine::middleware::outbound::{self, Request, Target};

type Rewrite = dyn Fn(&mut Request) -> Result<(), MiddlewareError> + Send + Sync;

struct OutboundMiddleware(Arc<Rewrite>);

impl std::fmt::Debug for OutboundMiddleware {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OutboundMiddleware")
    }
}

impl MiddlewarePlan for OutboundMiddleware {
    fn has_outbound(&self, _context: &MiddlewareContext) -> bool {
        true
    }

    fn handle_outbound(
        &self,
        context: MiddlewareContext,
        mut request: Request,
        next: outbound::Next,
    ) -> BoxFuture<'static, Result<Request, MiddlewareError>> {
        assert_eq!(context.mount(), MiddlewareMount::Outbound);
        let result = (self.0)(&mut request);
        Box::pin(async move {
            result?;
            next.run(request).await
        })
    }

    fn handle(
        &self,
        _context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        next.run(request)
    }
}

pub(in crate::provider) fn outbound_context(
    request_id: &str,
    rewrite: impl Fn(&mut Request) -> Result<(), MiddlewareError> + Send + Sync + 'static,
) -> AttemptContext {
    context_with_middleware(
        request_id,
        Arc::new(OutboundMiddleware(Arc::new(rewrite))),
        FastMode::Default,
    )
}

#[tokio::test]
async fn outbound_http_sends_final_encoded_bytes_and_target_without_restoring_defaults() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let original = MockServer::start().await;
    let target = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/plugin-target"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CAPTURE_COMPLETED_SSE),
        )
        .expect(1)
        .mount(&target)
        .await;
    let uri = format!("{}/plugin-target?source=outbound", target.uri());
    let mut stream = provider_with_base_url(&store, original.uri()).execute(
        planned_request("openai", http_generate_operation()),
        outbound_context("req_outbound_http", move |request| {
            assert!(matches!(&request.target, Target::Http { uri, .. } if uri.path() == "/codex/responses"));
            let headers = request.header_map().unwrap();
            assert!(headers.contains_key("user-agent"));
            assert_eq!(headers["content-encoding"], "zstd");
            let body = zstd::decode_all(request.body.as_ref()).unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["stream"], true);
            request.target = Target::Http { method: reqwest::Method::PATCH, uri: uri.parse().unwrap() };
            request.headers.retain(|header| !matches!(header.name(), "user-agent" | "content-encoding" | "content-type" | "authorization" | "accept"));
            request.headers.extend([
                MiddlewareHeader::new("x-plugin-values", Bytes::from_static(b"first")),
                MiddlewareHeader::new("x-plugin-values", Bytes::from_static(b"second")),
                MiddlewareHeader::new("x-plugin-bytes", Bytes::from_static(b"\x80")),
            ]);
            request.body = Bytes::from_static(b"\xff\0opaque outbound body");
            Ok(())
        }),
    ).await.unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    assert!(original.received_requests().await.unwrap().is_empty());
    let requests = target.received_requests().await.unwrap();
    let request = &requests[0];
    assert_eq!(request.body, b"\xff\0opaque outbound body");
    for name in [
        "user-agent",
        "content-encoding",
        "content-type",
        "authorization",
    ] {
        assert!(
            !request.headers.contains_key(name),
            "removed header restored: {name}"
        );
    }
    // 底层 reqwest 保留自身缺省 Accept，出站准备不承诺禁用该行为
    assert_eq!(request.headers["accept"], "*/*");
    assert_eq!(
        request
            .headers
            .get_all("x-plugin-values")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert_eq!(request.url.query(), Some("source=outbound"));
    assert_eq!(request.headers["x-plugin-bytes"].as_bytes(), b"\x80");
}

#[tokio::test]
async fn outbound_images_and_search_send_opaque_replacements() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[]})))
        .expect(3)
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    let raw = RawJsonPayload::new(
        "openai",
        Bytes::from_static(br#"{ "future":1, "future":2 }"#),
    )
    .unwrap();
    for operation in [
        Operation::GenerateImage(ImageRequest::from_raw_json(
            ImageRequestKind::Generation,
            raw.clone(),
        )),
        Operation::GenerateImage(ImageRequest::from_raw_json(
            ImageRequestKind::Edit,
            raw.clone(),
        )),
        Operation::Search(StandaloneSearchRequest::from_raw_json(raw)),
    ] {
        let mut stream = provider
            .clone()
            .execute(
                planned_provider_endpoint_request("openai", operation),
                outbound_context("req_outbound_json", |request| {
                    assert_eq!(request.body.as_ref(), br#"{ "future":1, "future":2 }"#);
                    assert!(!request.header_map().unwrap().contains_key("accept"));
                    let Target::Http { method, .. } = &mut request.target else {
                        panic!("HTTP target required")
                    };
                    *method = reqwest::Method::PUT;
                    request
                        .headers
                        .retain(|header| !matches!(header.name(), "user-agent" | "accept"));
                    request.body = Bytes::from_static(b"\xffendpoint bytes");
                    Ok(())
                }),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|r| r.url.path())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "/codex/images/generations",
            "/codex/images/edits",
            "/codex/alpha/search"
        ])
    );
    for request in requests {
        assert_eq!(request.body, b"\xffendpoint bytes");
        assert!(!request.headers.contains_key("user-agent"));
        assert_eq!(request.headers["accept"], "*/*");
    }
}

#[tokio::test]
async fn outbound_invalid_and_faulting_websocket_preparation_never_opens_or_falls_back() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider =
        provider_with_base_url(&store, format!("http://{}", listener.local_addr().unwrap()));
    for case in 0..6 {
        let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(
                "openai",
                json!({"model":"gpt-5.4", "input":"hello"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap()
            .with_context(Map::from_iter([("use_websocket".into(), json!(true))])),
        ));
        let mut stream = provider
            .clone()
            .execute(
                planned_request("openai", operation),
                outbound_context("req_outbound_invalid", move |request| {
                    assert!(matches!(request.target, Target::WebSocket { .. }));
                    match case {
                        0 => return Err(MiddlewareError::Fault),
                        1 => request.body = Bytes::from_static(b"\xff"),
                        2 => request.headers.push(MiddlewareHeader::new(
                            "x-invalid",
                            Bytes::from_static(b"bad\nheader"),
                        )),
                        3 => request.headers.retain(|header| {
                            !header.name().eq_ignore_ascii_case("sec-websocket-key")
                        }),
                        4 => request.headers.push(MiddlewareHeader::new(
                            "sec-websocket-extensions",
                            Bytes::from_static(b"unknown-extension"),
                        )),
                        5 => request.headers.push(MiddlewareHeader::new(
                            "x-non-ascii",
                            Bytes::from_static(b"\x80"),
                        )),
                        _ => unreachable!(),
                    }
                    Ok(())
                }),
            )
            .await
            .unwrap();
        let mut failure = None;
        timeout(Duration::from_secs(2), async {
            while let Some(event) = stream.next().await {
                if let Err(error) = event {
                    failure = Some(error);
                    break;
                }
            }
        })
        .await
        .unwrap();
        let failure = failure.expect("preparation must fail");
        assert_eq!(failure.send_state(), UpstreamSendState::NotSent);
        assert!(failure.retry_is_prohibited());
        if case != 0 {
            assert_eq!(failure.kind(), ProviderErrorKind::InvalidRequest);
        }
    }
    assert!(
        timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "preparation failure must not dial or fall back"
    );
}

#[tokio::test]
async fn outbound_websocket_keeps_exact_chains_and_reuses_only_matching_final_openings() {
    const ACCOUNT: &str = "acct_provider_contract";
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut first = accept_codex_test_websocket_with(stream, |request, _response| {
            assert_eq!(request.uri().path(), "/opening-a");
            assert!(!request.headers().contains_key("user-agent"));
            assert!(!request.headers().contains_key("sec-websocket-extensions"));
            assert_eq!(
                request
                    .headers()
                    .get_all("x-plugin-profile")
                    .iter()
                    .map(|v| v.to_str().unwrap())
                    .collect::<Vec<_>>(),
                ["a", "shared"]
            );
        })
        .await;
        for (index, response_id) in ["resp_outbound_a", "resp_outbound_exact"]
            .into_iter()
            .enumerate()
        {
            let text = timeout(Duration::from_secs(5), async {
                loop {
                    match first.next().await.unwrap().unwrap() {
                        Message::Text(text) => break text,
                        Message::Ping(payload) => first.send(Message::Pong(payload)).await.unwrap(),
                        message => panic!("unexpected frame: {message:?}"),
                    }
                }
            })
            .await
            .unwrap();
            let body: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(body["plugin_turn"], index);
            if index == 1 {
                assert_eq!(body["previous_response_id"], "plugin-forged-reference");
            }
            first.send(Message::Text(json!({"type":"response.completed", "response":{"id":response_id,"model":"gpt-5.4","status":"completed","output":[],"store":false}}).to_string().into())).await.unwrap();
        }
        let (stream, _) = timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut second = accept_codex_test_websocket_with(stream, |request, _response| {
            assert_eq!(request.uri().path(), "/opening-b");
            assert!(!request.headers().contains_key("sec-websocket-extensions"));
            assert_eq!(
                request
                    .headers()
                    .get_all("x-plugin-profile")
                    .iter()
                    .map(|v| v.to_str().unwrap())
                    .collect::<Vec<_>>(),
                ["b", "shared"]
            );
        })
        .await;
        for index in 2..4 {
            let text = timeout(Duration::from_secs(5), async {
                loop {
                    match second.next().await.unwrap().unwrap() {
                        Message::Text(text) => break text,
                        Message::Ping(payload) => {
                            second.send(Message::Pong(payload)).await.unwrap()
                        }
                        message => panic!("unexpected frame: {message:?}"),
                    }
                }
            })
            .await
            .unwrap();
            if index == 3 {
                assert_eq!(text.as_str(), "opaque text from plugin");
            } else {
                assert_eq!(
                    serde_json::from_str::<Value>(&text).unwrap()["plugin_turn"],
                    index
                );
            }
            second.send(Message::Text(json!({"type":"response.completed", "response":{"id":format!("resp_outbound_{index}"),"model":"gpt-5.4","status":"completed","output":[],"store":false}}).to_string().into())).await.unwrap();
        }
        // 第一条连接直到新 profile 完成仍保持活动，排除断开触发重新拨号的假阳性
        drop(first);
    });
    let provider = provider_with_base_url(&store, format!("http://{address}"));
    for index in 0..4 {
        let previous = (index == 1).then_some("resp_outbound_a");
        let mut body = json!({"model":"gpt-5.4","input":"hello","store":false});
        if let Some(previous) = previous {
            body["previous_response_id"] = json!(previous);
        }
        let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object("openai", body.as_object().unwrap().clone()).unwrap().with_context(Map::from_iter([
                ("use_websocket".into(), json!(true)),
                ("downstream_websocket_connection_id".into(), json!("outbound-downstream")),
            ])),
        ).with_provider_session_state(ProviderSessionState::new("openai", json!({"account_id":ACCOUNT,"conversation_id":"outbound-conversation","continuation_scope":"connection_local"}).as_object().unwrap().clone()).unwrap()));
        if index == 1 {
            let mut failed = provider
                .clone()
                .execute(
                    planned_request("openai", operation.clone()),
                    outbound_context("req_outbound_pool_fault", |_| Err(MiddlewareError::Fault)),
                )
                .await
                .unwrap();
            let mut error = None;
            while let Some(event) = failed.next().await {
                if let Err(failure) = event {
                    error = Some(failure);
                    break;
                }
            }
            assert_eq!(error.unwrap().send_state(), UpstreamSendState::NotSent);
        }
        let middleware = FrozenMiddlewarePlan::new(
            Arc::new(OutboundMiddleware(Arc::new(move |request| {
                assert!(
                    request
                        .header_map()
                        .unwrap()
                        .contains_key("sec-websocket-key")
                );
                let Target::WebSocket { uri } = &mut request.target else {
                    panic!("WebSocket target")
                };
                *uri = match index {
                    0 => format!("ws://{address}/opening-a"),
                    1 => "ws://127.0.0.1:1/must-not-open".to_owned(),
                    _ => format!("ws://{address}/opening-b"),
                }
                .parse()
                .unwrap();
                request.headers.retain(|header| {
                    !["user-agent", "sec-websocket-extensions"]
                        .iter()
                        .any(|name| header.name().eq_ignore_ascii_case(name))
                });
                request.headers.push(MiddlewareHeader::new(
                    "x-plugin-profile",
                    Bytes::from_static(if index == 0 { b"a" } else { b"b" }),
                ));
                request.headers.push(MiddlewareHeader::new(
                    "x-plugin-profile",
                    Bytes::from_static(b"shared"),
                ));
                let mut body: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(body["type"], "response.create");
                body["plugin_turn"] = json!(index);
                if index == 1 {
                    body["previous_response_id"] = json!("plugin-forged-reference");
                }
                request.body = if index == 3 {
                    Bytes::from_static(b"opaque text from plugin")
                } else {
                    serde_json::to_vec(&body).unwrap().into()
                };
                Ok(())
            }))),
            ExtensionSetReference::new(
                ExtensionSetId::new(format!("outbound-{index}")).unwrap(),
                Arc::new(TestExtensionLease),
            ),
        );
        let provider_kind = ProviderKind::new("openai").unwrap();
        let account = ProviderAccountId::new(ACCOUNT).unwrap();
        let client = ClientApiKeyId::new("key_openai_contract").unwrap();
        let binding = previous.map(|previous| {
            ContinuationBinding::Pinned(
                NativeContinuationPin::new(
                    PreviousResponseId::new(previous),
                    PreviousResponseId::new(previous),
                    client.clone(),
                    provider_kind.clone(),
                    account.clone(),
                )
                .with_scope(
                    gateway_core::engine::continuation::NativeContinuationScope::ConnectionLocal,
                ),
            )
        });
        let attempt = AttemptContext::new(
            RequestAttemptContext::new(
                ModelRequestId::new(format!("req_outbound_ws_{index}")).unwrap(),
                client,
            )
            .with_middleware(
                Some(middleware),
                Arc::from([]),
                "/v1/responses".into(),
                ClientTransport::HttpSse,
            ),
            NonZeroU32::MIN,
            SystemTime::now() + Duration::from_secs(10),
            account_policy(),
            AccountAttemptContext::new(
                BTreeSet::new(),
                None,
                Some(ProviderAccountStateOwner::new(provider_kind, account)),
            )
            .with_account_scope(contract_account_scope()),
            binding,
            CancellationToken::new(),
        )
        .with_continuation_attempt(if previous.is_some() {
            ContinuationAttempt::Native
        } else {
            ContinuationAttempt::None
        });
        let mut stream = provider
            .clone()
            .execute(planned_request("openai", operation), attempt)
            .await
            .unwrap();
        timeout(Duration::from_secs(10), async {
            while let Some(event) = stream.next().await {
                event.unwrap();
            }
        })
        .await
        .unwrap();
    }
    timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}
