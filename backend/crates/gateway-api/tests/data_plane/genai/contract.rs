//! 跨模块行为合同测试

use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};

#[tokio::test]
async fn session_headers_reach_provider_context_except_when_declared_hop_by_hop() {
    for connection in [None, Some("x-http-session-id")] {
        let execution = Arc::new(Execution {
            session: Mutex::new(Some(Session::new(vec![ProviderEvent::wire(
                ProtocolWireEvent::raw_json(
                    GENAI_PROTOCOL,
                    Bytes::from_static(br#"{"candidates":[]}"#),
                )
                .unwrap(),
            )]))),
            ..Default::default()
        });
        let router = api_router(execution.clone()).await;
        let mut req = request(&format!("{SHORT}:generateContent"));
        req.headers_mut()
            .insert("x-http-session-id", "native-session".parse().unwrap());
        req.headers_mut()
            .insert("x-session-id", "fallback-session".parse().unwrap());
        if let Some(value) = connection {
            req.headers_mut()
                .insert("connection", value.parse().unwrap());
        }
        let response = router.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let captured = execution.seen.lock().unwrap().take().unwrap();
        let Operation::Generate(op) = captured.operation else {
            panic!("generation")
        };
        let headers = &op.protocol_payload().context()["headers"];
        assert_eq!(
            headers.get("x-http-session-id").and_then(Value::as_str),
            connection.is_none().then_some("native-session")
        );
        assert_eq!(headers["x-session-id"], "fallback-session");
    }
}

#[tokio::test]
async fn native_tool_call_and_client_replay_keep_the_original_wire() {
    let raw = Bytes::from_static(br#"{ "candidates":[{"finishReason":"STOP","content":{"role":"model","parts":[{"functionCall":{"name":"read","args":{"id":42}},"thoughtSignature":"c2ln","futurePart":true}]}}],"futureResponse":1 }"#);
    let event =
        ProviderEvent::wire(ProtocolWireEvent::raw_json(GENAI_PROTOCOL, raw.clone()).unwrap());
    let execution = Arc::new(Execution {
        session: Mutex::new(Some(Session::new(vec![event]))),
        ..Default::default()
    });
    let router = api_router(execution.clone()).await;
    let response = router
        .clone()
        .oneshot(request(&format!("{SHORT}:generateContent")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let first_body = to_bytes(response.into_body(), 2048).await.unwrap();
    assert_eq!(first_body, raw);
    let captured = execution.seen.lock().unwrap().take().unwrap();
    assert_eq!(captured.public_model.as_str(), "model-a");
    let Operation::Generate(op) = captured.operation else {
        panic!("expected model operation")
    };
    let p = op.protocol_payload();
    assert_eq!(p.raw_json_body().unwrap().as_ref(), BODY.as_bytes());
    let context = json!({
        "stream": false,
        "headers": {"user-agent": "fixture-client/1.0", "x-goog-api-client": "fixture-transport/1.0"},
        "opaque_request_headers": [
            ["x-goog-api-client", STANDARD.encode("fixture-transport/1.0")],
            ["x-goog-user-project", STANDARD.encode("untrusted")]
        ]
    });
    assert_eq!(p.context(), context.as_object().unwrap());

    let first: Value = serde_json::from_slice(&first_body).unwrap();
    let mut history: Value = serde_json::from_str(BODY).unwrap();
    history["contents"].as_array_mut().unwrap().extend([
        first["candidates"][0]["content"].clone(),
        json!({"role":"user","parts":[{"functionResponse":{"name":"read","response":{"value":42}}}]}),
    ]);
    let followup = serde_json::to_vec_pretty(&history).unwrap();
    let reply = Bytes::from_static(
        br#"{ "candidates":[{"finishReason":"STOP","content":{"parts":[{"text":"42"}]}}] }"#,
    );
    *execution.session.lock().unwrap() = Some(Session::new(vec![ProviderEvent::wire(
        ProtocolWireEvent::raw_json(GENAI_PROTOCOL, reply.clone()).unwrap(),
    )]));
    let mut next = request(&format!("{SHORT}:generateContent"));
    *next.body_mut() = Body::from(followup.clone());
    let response = router.oneshot(next).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(to_bytes(response.into_body(), 2048).await.unwrap(), reply);
    let captured = execution.seen.lock().unwrap().take().unwrap();
    assert_eq!(
        captured.operation.middleware_body().unwrap().as_ref(),
        followup
    );
}

#[tokio::test]
async fn business_headers_keep_multi_value_bytes_without_credentials_or_hop_by_hop_fields() {
    let execution = Arc::new(Execution {
        session: Mutex::new(Some(Session::new(vec![ProviderEvent::wire(
            ProtocolWireEvent::raw_json(
                GENAI_PROTOCOL,
                Bytes::from_static(br#"{"candidates":[]}"#),
            )
            .unwrap(),
        )]))),
        ..Default::default()
    });
    let router = api_router(execution.clone()).await;
    let mut req = request(&format!("{SHORT}:generateContent"));
    for (name, value) in [
        ("x-business", b"one\x80".as_slice()),
        ("x-business", b"two"),
        ("connection", b"x-hop"),
        ("x-hop", b"private"),
        ("cookie", b"private"),
        ("x-goog-api-key", b"private"),
        ("traceparent", b"original-trace"),
    ] {
        req.headers_mut()
            .append(name, axum::http::HeaderValue::from_bytes(value).unwrap());
    }
    assert_eq!(router.oneshot(req).await.unwrap().status(), StatusCode::OK);
    let captured = execution.seen.lock().unwrap().take().unwrap();
    let Operation::Generate(op) = captured.operation else {
        panic!("generation")
    };
    let headers = op.protocol_payload().context()["opaque_request_headers"]
        .as_array()
        .unwrap();
    let values: Vec<_> = headers
        .iter()
        .filter(|pair| pair[0] == "x-business")
        .map(|pair| STANDARD.decode(pair[1].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(values, vec![b"one\x80".to_vec(), b"two".to_vec()]);
    for name in [
        "authorization",
        "cookie",
        "x-goog-api-key",
        "connection",
        "x-hop",
        "x-request-id",
    ] {
        assert!(
            !headers.iter().any(|pair| pair[0] == name),
            "unexpected {name}"
        );
    }
    assert!(
        headers
            .iter()
            .any(|pair| pair == &json!(["traceparent", STANDARD.encode("original-trace")]))
    );
}

#[tokio::test]
async fn committed_stream_delivers_raw_error_once() {
    let first = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]}}]}\r\n\r\n";
    let last = "data: { \"error\":{\"code\":429,\"status\":\"RESOURCE_EXHAUSTED\",\"message\":\"slow down\"} }\r\n\r\n";
    let mut session = Session::new(vec![wire_frame(first), wire_frame(last)]);
    session.finish = Finish::Error;
    let execution = Arc::new(Execution {
        session: Mutex::new(Some(session)),
        ..Default::default()
    });
    let response = api_router(execution)
        .await
        .oneshot(request(&format!("{SHORT}:streamGenerateContent?alt=sse")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), 2048).await.unwrap().as_ref(),
        [first, last].concat().as_bytes()
    );
}

#[tokio::test]
async fn stream_preserves_finish_and_later_usage_bytes_before_eof() {
    let first = "data: { \"candidates\":[{\"finishReason\":\"STOP\"}], \"error\":null }\r\n\r\n";
    let last = "data: {\"usageMetadata\":{\"totalTokenCount\":7}}\r\n\r\n";
    let mut events = vec![wire_frame(first), wire_frame(last)];
    events.push(ProviderEvent::canonical(GatewayEvent::Completed(
        ResponseMeta::new("id", "model-a"),
    )));
    let session = Session::new(events);
    let trace = session.trace.clone();
    let execution = Arc::new(Execution {
        session: Mutex::new(Some(session)),
        ..Default::default()
    });
    let response = api_router(execution)
        .await
        .oneshot(request(&format!("{SHORT}:streamGenerateContent?alt=sse")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), 2048).await.unwrap().as_ref(),
        [first, last].concat().as_bytes()
    );
    assert_eq!(trace.commits.load(Ordering::SeqCst), 1);
    assert!(trace.finalized.load(Ordering::SeqCst));
}

#[tokio::test]
async fn metric_validates_bearer_and_invalid_generation_never_starts_inference() {
    let execution = Arc::new(Execution::default());
    let router = api_router(execution.clone()).await;
    for (uri, authenticated, status) in [
        (
            "/v1beta:recordCodeAssistMetrics",
            false,
            StatusCode::UNAUTHORIZED,
        ),
        ("/v1beta:recordCodeAssistMetrics", true, StatusCode::OK),
        (
            "/v1beta/models/model:streamGenerateContent?alt=json",
            true,
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1beta/models/a%2Fb:generateContent",
            true,
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let mut req = request(uri);
        if !authenticated {
            req.headers_mut().remove("authorization");
        }
        let res = router.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), status);
        let value: Value =
            serde_json::from_slice(&to_bytes(res.into_body(), 2048).await.unwrap()).unwrap();
        if status.is_success() {
            assert_eq!(value, json!({}));
        } else {
            assert_eq!(value["error"]["code"], status.as_u16());
        }
    }
    assert!(execution.seen.lock().unwrap().is_none());
}

#[tokio::test]
async fn unknown_paths_and_methods_return_google_rpc_errors() {
    let router = api_router(Arc::new(Execution::default())).await;
    for (request, status, rpc_status) in [
        (
            request("/v1beta/does-not-exist"),
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
        ),
        (
            Request::get(format!("{SHORT}:generateContent"))
                .body(Body::empty())
                .unwrap(),
            StatusCode::METHOD_NOT_ALLOWED,
            "INVALID_ARGUMENT",
        ),
    ] {
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), status);
        let bytes = to_bytes(response.into_body(), 1024).await.unwrap();
        let payload: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(payload["error"]["code"], status.as_u16());
        assert_eq!(payload["error"]["status"], rpc_status);
    }
}

#[tokio::test]
async fn count_preserves_native_wire_and_uses_url_model_without_streaming() {
    let raw = Bytes::from_static(br#"{ "totalTokens":42,"cachedContentTokenCount":7 }"#);
    let execution = Arc::new(Execution {
        session: Mutex::new(Some(Session::new(vec![ProviderEvent::wire(
            ProtocolWireEvent::raw_json(GENAI_PROTOCOL, raw.clone()).unwrap(),
        )]))),
        ..Default::default()
    });
    let response = api_router(execution.clone())
        .await
        .oneshot(request(&format!("{SHORT}:countTokens")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(to_bytes(response.into_body(), 2048).await.unwrap(), raw);
    let seen = execution.seen.lock().unwrap().take().unwrap();
    assert_eq!(seen.public_model.as_str(), "model-a");
    assert!(!seen.metadata.stream);
    let Operation::CountTokens(count) = seen.operation else {
        panic!("expected token count")
    };
    assert!(count.requires_native_protocol());
    assert_eq!(count.payload().body().as_ref(), BODY.as_bytes());
}

#[tokio::test]
async fn invalid_count_result_is_not_a_successful_response() {
    for raw in [
        br#"{}"#.as_slice(),
        br#"{"totalTokens":-1}"#,
        br#"{"totalTokens":"42"}"#,
    ] {
        let execution = Arc::new(Execution {
            session: Mutex::new(Some(Session::new(vec![ProviderEvent::wire(
                ProtocolWireEvent::raw_json(GENAI_PROTOCOL, Bytes::copy_from_slice(raw)).unwrap(),
            )]))),
            ..Default::default()
        });
        let response = api_router(execution)
            .await
            .oneshot(request(&format!("{SHORT}:countTokens")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 2048).await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], 500);
    }
}

#[tokio::test]
async fn count_rejects_sse_before_execution() {
    let execution = Arc::new(Execution::default());
    let response = api_router(execution.clone())
        .await
        .oneshot(request(&format!("{SHORT}:countTokens?alt=sse")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(execution.seen.lock().unwrap().is_none());
}
