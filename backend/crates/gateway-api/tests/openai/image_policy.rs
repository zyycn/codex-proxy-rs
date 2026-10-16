//! 真实 HTTP / WebSocket 路由与 Core 执行入口共同拒绝生图，不触发准入或上游

use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::AUTHORIZATION},
};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use tower::ServiceExt;

fn execution() -> Arc<DefaultExecutionService> {
    Arc::new(DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(snapshot_with_image_policy(
            "sk_image_policy",
            "openai",
            "key_image_policy",
            &["acct_api_test"],
            false,
        )),
        Arc::new(UnusedExecutionStore),
        ProviderRegistry::default(),
        Arc::new(UnusedAdmissions),
        Arc::new(UnusedContinuation),
        Arc::new(IgnoredClientApiKeyUsage),
        Arc::new(crate::support::RecordingDiagnostics::default()),
    ))
}

#[tokio::test]
async fn denied_image_requests_return_403_before_admission_or_upstream() {
    let app = api_router(execution()).await;
    for (path, body) in [
        (
            "/v1/images/generations",
            json!({"model":"gpt-image-2","prompt":"test"}),
        ),
        (
            "/v1/images/edits",
            json!({"model":"gpt-image-2","prompt":"test","images":[{"image_url":"data:image/png;base64,AA=="}]}),
        ),
        (
            "/v1/responses",
            json!({"model":"model-a","input":"test","tools":[{"type":"image_generation"}]}),
        ),
        (
            "/v1/responses",
            json!({"model":"model-a","input":"test","stream":true,"tool_choice":{"type":"image_generation"}}),
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::post(path)
                    .header(AUTHORIZATION, "Bearer sk_image_policy")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
        assert_eq!(body["error"]["code"], "image_generation_disabled");
    }
}

#[tokio::test]
async fn each_websocket_create_enforces_image_policy() {
    let app = api_router(execution()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut request = format!("ws://{address}/v1/responses")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(AUTHORIZATION, "Bearer sk_image_policy".parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    for image_fields in [
        json!({"tools":[{"type":"image_generation"}]}),
        json!({"tool_choice":{"type":"image_generation"}}),
    ] {
        let mut body = json!({"type":"response.create","model":"model-a","input":"test"});
        body.as_object_mut()
            .unwrap()
            .extend(image_fields.as_object().unwrap().clone());
        socket
            .send(Message::Text(body.to_string().into()))
            .await
            .unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let body: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        assert_eq!(body["type"], "error", "{body}");
        assert_eq!(body["status"], 403);
        assert_eq!(body["error"]["code"], "image_generation_disabled");
    }
    socket.close(None).await.unwrap();
    server.abort();
}
