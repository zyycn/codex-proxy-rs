use serde_json::json;
use wiremock::{
    matchers::{body_json, header, method, path},
    Mock, MockServer, ResponseTemplate,
};

use codex_proxy_rs::{
    codex::gateway::fingerprint::model::Fingerprint,
    codex::gateway::transport::{
        http_client::{build_reqwest_client, CodexBackendClient, CodexRequestContext},
        types::CodexResponsesRequest,
        usage_events::TokenUsage,
    },
};

#[tokio::test]
async fn codex_backend_client_should_send_desktop_headers_and_capture_response_metadata() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":2,\"output_tokens\":3,\"input_tokens_details\":{\"cached_tokens\":1}}}}\n",
        "\n",
    );
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-token"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .and(header("originator", "Codex Desktop"))
        .and(header("x-client-request-id", "req_1"))
        .and(header("x-codex-turn-state", "turn_1"))
        .and(header("cookie", "cf_clearance=old"))
        .and(body_json(json!({
            "model": "gpt-5.5",
            "instructions": "",
            "input": [],
            "stream": true,
            "store": false
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header(
                    "set-cookie",
                    "cf_clearance=new; Domain=.chatgpt.com; Path=/",
                )
                .insert_header("x-codex-turn-state", "turn_2")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        server.uri(),
        Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &CodexResponsesRequest::new_http_sse("gpt-5.5", "", Vec::new()),
            CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_1",
                turn_state: Some("turn_1"),
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
                codex_window_id: None,
                parent_thread_id: None,
                cookie_header: Some("cf_clearance=old"),
                installation_id: None,
                session_id: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        response.usage,
        Some(TokenUsage {
            input_tokens: 2,
            output_tokens: 3,
            cached_tokens: 1,
            total_tokens: 5,
        })
    );
    assert_eq!(response.turn_state.as_deref(), Some("turn_2"));
    assert_eq!(
        response.set_cookie_headers,
        vec!["cf_clearance=new; Domain=.chatgpt.com; Path=/".to_string()]
    );
}
