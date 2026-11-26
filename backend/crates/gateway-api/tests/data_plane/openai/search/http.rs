//! 对应生产职责的行为测试

use super::*;

#[tokio::test]
async fn search_route_should_preserve_request_and_success_response_bytes() {
    let mut execution = SearchExecution::new(false);
    let middleware = Arc::new(crate::data_plane::middleware::RequestMiddleware {
        expected_operation: Some(OperationKind::Search),
        committed_statuses: Some(execution.committed_statuses.clone()),
        ..Default::default()
    });
    Arc::get_mut(&mut execution).unwrap().middleware = Some(middleware.clone());
    let request_body = br#"{ "id":"search-session", "model":"gpt-future", "commands":{"search_query":[{"q":"private query"}]}, "future":1, "future":2 }"#;
    let turn_metadata =
        r#"{"session_id":"session","thread_id":"thread","turn_id":"turn","future":true}"#;
    let response = api_router(execution.clone())
        .await
        .oneshot(
            Request::post("/v1/alpha/search")
                .header(AUTHORIZATION, "Bearer sk_search_test")
                .header("content-type", "application/json")
                .header("x-codex-turn-metadata", turn_metadata)
                .header("session-id", "root-session")
                .header("thread-id", "child-thread")
                .header("x-future-business", "first")
                .header("x-future-business", "second")
                .header("cookie", "client-secret")
                .header("connection", "x-private-hop")
                .header("x-private-hop", "client-hop")
                .body(Body::from(request_body.to_vec()))
                .expect("search request"),
        )
        .await
        .expect("search response");

    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers()["x-request-middleware"], "applied");
    assert_eq!(
        *middleware.endpoints.lock().unwrap(),
        vec!["/v1/alpha/search"]
    );
    assert_eq!(response.headers()["x-request-id"], "req_search_test");
    assert_eq!(
        response.headers()["x-gateway-request-id"],
        "req_search_test"
    );
    assert_eq!(
        response.headers().get("x-search-rate-limit"),
        Some(&"42".parse().expect("header value"))
    );
    let response_body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read search response");
    assert_eq!(response_body.as_ref(), SEARCH_RESPONSE);

    let captured = execution.captured();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].provider, "openai");
    assert_eq!(captured[0].endpoint, "/v1/alpha/search");
    assert_eq!(captured[0].transport, ClientTransport::HttpJson);
    assert_eq!(captured[0].body.as_ref(), request_body);
    let mut context = captured[0].context.clone();
    let headers = context
        .as_object_mut()
        .unwrap()
        .remove("opaque_request_headers")
        .unwrap();
    let headers: Vec<_> = headers
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry[0].as_str().unwrap(),
                STANDARD.decode(entry[1].as_str().unwrap()).unwrap(),
            )
        })
        .collect();
    assert_eq!(
        headers
            .iter()
            .filter(|(name, _)| *name == "x-future-business")
            .map(|(_, value)| value.as_slice())
            .collect::<Vec<_>>(),
        [b"first".as_slice(), b"second".as_slice()]
    );
    assert!(!headers.iter().any(|(name, _)| matches!(
        *name,
        "authorization" | "cookie" | "connection" | "x-private-hop"
    )));
    assert_eq!(
        context,
        json!({"turn_metadata": turn_metadata, "session_id": "root-session", "thread_id": "child-thread"})
    );
    assert_eq!(
        *execution
            .committed_statuses
            .lock()
            .expect("committed status lock"),
        vec![StatusCode::CREATED.as_u16()]
    );
}

#[tokio::test]
async fn search_route_should_return_the_exact_upstream_error_response() {
    let response = api_router(SearchExecution::new(true))
        .await
        .oneshot(
            Request::post("/v1/alpha/search")
                .header(AUTHORIZATION, "Bearer sk_search_test")
                .header("content-type", "application/json")
                .body(Body::from(
                    br#"{ "model":"gpt-future", "future_invalid":9007199254740993 }"#.to_vec(),
                ))
                .expect("search request"),
        )
        .await
        .expect("search response");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response.headers()["x-request-id"], "req_search_test");
    assert_eq!(
        response.headers()["x-gateway-request-id"],
        "req_search_test"
    );
    assert_eq!(
        response.headers().get("content-type"),
        Some(&"application/problem+json".parse().expect("content type"))
    );
    assert_eq!(
        response.headers().get("x-future-search-error"),
        Some(&"preserved".parse().expect("header value"))
    );
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read search response");
    assert_eq!(
        body.as_ref(),
        br#"{ "error":{"message":"future search validation"}, "future":9007199254740993 }"#
    );
}
