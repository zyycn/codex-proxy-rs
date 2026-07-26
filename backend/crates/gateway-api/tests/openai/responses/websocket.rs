use axum::{
    body::Body,
    http::{Request, StatusCode, header::AUTHORIZATION},
};
use gateway_api::openai::responses::ResponseCreateFrameError;
use gateway_core::operation::Operation;
use serde_json::json;
use tower::ServiceExt;

use super::decode_response_create;
use crate::openai::{api_router, models::ModelsExecution};

#[test]
fn response_create_should_default_to_the_websocket_streaming_contract() {
    let decoded = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": "hello",
            "store": false
        })
        .to_string(),
    )
    .expect("decode response.create");

    assert!(decoded.metadata().stream());
    assert!(!decoded.metadata().store());
    let Operation::Generate(request) = decoded.operation() else {
        panic!("Responses must map to Generate");
    };
    assert!(request.protocol_payload().body().get("stream").is_none());
}

#[test]
fn response_create_should_preserve_provider_options_as_opaque_wire_body() {
    let decoded = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": "hello",
            "stream": true,
            "provider_options": {
                "version": "v1",
                "providers": {
                    "openai": {"schema_version": 1, "transport": "websocket"}
                }
            }
        })
        .to_string(),
    )
    .expect("decode opaque provider options");
    let Operation::Generate(request) = decoded.operation() else {
        panic!("Responses must map to Generate");
    };
    let payload = request.protocol_payload();

    assert_eq!(
        payload.body().get("provider_options"),
        Some(&json!({
            "version": "v1",
            "providers": {
                "openai": {"schema_version": 1, "transport": "websocket"}
            }
        }))
    );
    assert!(payload.context().get("provider_options").is_none());
}

#[test]
fn response_create_should_preserve_compaction_trigger_for_openai() {
    let decoded = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": [
                {"type": "message", "role": "user", "content": "history"},
                {"type": "compaction_trigger"}
            ]
        })
        .to_string(),
    )
    .expect("decode OpenAI response.create");
    let Operation::Generate(request) = decoded.operation() else {
        panic!("OpenAI response.create must remain Generate");
    };

    assert_eq!(
        request
            .protocol_payload()
            .body()
            .get("input")
            .and_then(|input| input.pointer("/1/type")),
        Some(&json!("compaction_trigger"))
    );
}

#[test]
fn response_create_should_reject_explicit_non_streaming_requests() {
    let error = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": "hello",
            "stream": false
        })
        .to_string(),
    )
    .expect_err("WebSocket requests must stream");

    assert_eq!(error, ResponseCreateFrameError::StreamingRequired);
}

#[test]
fn response_create_should_reject_invalid_frame_shapes() {
    for (payload, expected) in [
        ("not-json", ResponseCreateFrameError::InvalidJson),
        ("[]", ResponseCreateFrameError::ExpectedObject),
        (
            r#"{"type":"future.message","model":"smart-code","input":"hello"}"#,
            ResponseCreateFrameError::UnsupportedType,
        ),
    ] {
        assert_eq!(
            decode_response_create(payload).expect_err("invalid frame"),
            expected
        );
    }
}

#[test]
fn response_create_should_reject_non_boolean_stream_without_disclosing_body_values() {
    let prompt = "private-websocket-prompt-marker";
    let opaque_stream_value = "private-websocket-option-marker";
    let error = decode_response_create(
        &json!({
            "type": "response.create",
            "model": "smart-code",
            "input": prompt,
            "stream": opaque_stream_value
        })
        .to_string(),
    )
    .expect_err("WebSocket response.create must explicitly enable streaming");
    let rendered = format!("{error:?}\n{error}");

    assert_eq!(error, ResponseCreateFrameError::StreamingRequired);
    assert!(!rendered.contains(prompt));
    assert!(!rendered.contains(opaque_stream_value));
}

// `serve_responses_websocket`/`forward_execution` 只有拿到 `hyper::upgrade::OnUpgrade`
// 的真实 HTTP/1.1 升级连接才会运行;本 crate 的依赖图按 axum `ws` feature 编译
// hyper(无 `http1`),测试进程内不存在能完成升级的服务端,`hyper::upgrade` 也不
// 提供公开构造器。集成测试因此止步于 upgrade 边界:会话循环内的事件转发、错误帧
// 与断连取消仍未被覆盖,需要 dev 依赖开启 `http1` 或 socket 泛化后才能落地。

const TEST_WEBSOCKET_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAA==";

fn upgrade_request(authorization: &str) -> Request<Body> {
    Request::get("/v1/responses")
        .header(AUTHORIZATION, authorization)
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", TEST_WEBSOCKET_KEY)
        .body(Body::empty())
        .expect("build WebSocket upgrade request")
}

#[tokio::test]
async fn get_responses_should_route_to_the_websocket_upgrade_boundary() {
    let response = api_router(ModelsExecution::new())
        .await
        .oneshot(upgrade_request("Bearer sk_models_test"))
        .await
        .expect("route WebSocket upgrade request");

    // oneshot 请求不携带升级状态;426 证明 GET /v1/responses 进入的是
    // WebSocketUpgrade 边界而不是普通 HTTP handler。
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
}

#[tokio::test]
async fn websocket_upgradability_should_be_checked_before_authentication() {
    let response = api_router(ModelsExecution::new())
        .await
        .oneshot(upgrade_request("Bearer sk_invalid"))
        .await
        .expect("route unauthenticated upgrade request");

    // 升级能力在 extractor 阶段先于 handler 内的 API Key 认证被校验,
    // 无效凭据得到的仍是升级失败而不是 401。
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
}

#[tokio::test]
async fn websocket_upgrade_should_reject_malformed_handshakes() {
    let missing_connection = Request::get("/v1/responses")
        .header(AUTHORIZATION, "Bearer sk_models_test")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", TEST_WEBSOCKET_KEY)
        .body(Body::empty())
        .expect("build request without connection header");
    let unsupported_version = Request::get("/v1/responses")
        .header(AUTHORIZATION, "Bearer sk_models_test")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "12")
        .header("sec-websocket-key", TEST_WEBSOCKET_KEY)
        .body(Body::empty())
        .expect("build request with unsupported version");
    let missing_key = Request::get("/v1/responses")
        .header(AUTHORIZATION, "Bearer sk_models_test")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .body(Body::empty())
        .expect("build request without websocket key");

    for request in [missing_connection, unsupported_version, missing_key] {
        let response = api_router(ModelsExecution::new())
            .await
            .oneshot(request)
            .await
            .expect("route malformed WebSocket handshake");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
