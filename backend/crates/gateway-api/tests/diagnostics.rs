//! HTTP 入口拒绝可检索，诊断摘要不保存凭据和请求内容

use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::support::RecordingDiagnostics;

async fn fixture() -> (axum::Router, Arc<RecordingDiagnostics>) {
    let admin = crate::admin::AdminTestFixture::new().await;
    let diagnostics = Arc::new(RecordingDiagnostics::default());
    let router = crate::openai::api_bundle_with_diagnostics(
        admin.services,
        gateway_api::ApiConfig {
            asset_directory: env!("CARGO_MANIFEST_DIR").into(),
            cors_allowed_origins: Vec::new(),
            request_timeout_seconds: None,
            request_id_header: "x-correlation-id".into(),
        },
        diagnostics.clone(),
    )
    .router();
    (router, diagnostics)
}

#[tokio::test]
async fn decode_rejections_record_the_ingress_id_and_safe_parameter_without_model_facts() {
    for (path, body, field) in [
        (
            "/v1/chat/completions",
            json!({"model":"model-a", "messages":[{"role":"user", "content":[{"type":"SENSITIVE_CONTENT"}]}]}),
            "messages[0].content[0].type",
        ),
        (
            "/v1/responses",
            json!({"model":42, "input":"SENSITIVE_CONTENT"}),
            "model",
        ),
    ] {
        let (router, diagnostics) = fixture().await;
        let response = router
            .oneshot(
                Request::post(path)
                    .header("authorization", "Bearer unused-client-route-key")
                    .header("x-correlation-id", "ingress-test-608")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()["x-correlation-id"], "ingress-test-608");
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["error"]["param"], field);
        let failures = diagnostics.0.lock().unwrap();
        assert_eq!(failures.len(), 1);
        let failure = &failures[0];
        assert_eq!(failure.component, "api");
        assert_eq!(failure.operation, "http_ingress");
        assert_eq!(failure.correlation_id.as_deref(), Some("ingress-test-608"));
        let summary: Value = serde_json::from_str(&failure.message).unwrap();
        assert_eq!(summary["param"], field);
        assert_eq!(summary["endpoint"], path);
        assert!(failure.upstream_status.is_none());
        assert!(failure.account_id.is_none());
        assert!(!failure.message.contains("SENSITIVE_CONTENT"));
        assert!(!failure.message.contains("unused-client-route-key"));
    }
}

#[tokio::test]
async fn authentication_unknown_routes_and_method_errors_are_observable() {
    for (method, path, status, code) in [
        (
            "POST",
            "/v1/chat/completions",
            StatusCode::UNAUTHORIZED,
            "missing_authorization",
        ),
        (
            "POST",
            "/v1/unknown?secret=SENSITIVE_QUERY",
            StatusCode::NOT_FOUND,
            "unknown_endpoint",
        ),
        (
            "DELETE",
            "/v1/chat/completions",
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
        ),
    ] {
        let (router, diagnostics) = fixture().await;
        let response = router
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::from("SENSITIVE_CONTENT"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        let failures = diagnostics.0.lock().unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].kind, code);
        assert!(!failures[0].message.contains("SENSITIVE"));
        assert!(failures[0].correlation_id.is_some());
    }
}

#[derive(Clone)]
struct Recorder {
    dump_enabled: bool,
    events: Arc<std::sync::Mutex<Vec<std::collections::BTreeMap<String, String>>>>,
}

impl tracing::Subscriber for Recorder {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() != "request_dump" || self.dump_enabled
    }
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields(std::collections::BTreeMap<String, String>);
        impl tracing::field::Visit for Fields {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                self.0.insert(field.name().to_owned(), value.to_owned());
            }
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.insert(field.name().to_owned(), format!("{value:?}"));
            }
        }
        let mut fields = Fields(std::collections::BTreeMap::new());
        event.record(&mut fields);
        fields
            .0
            .insert("target".into(), event.metadata().target().into());
        self.events.lock().unwrap().push(fields.0);
    }
}

#[tokio::test]
async fn original_rejected_request_is_dumped_only_when_explicitly_enabled() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use tracing::instrument::WithSubscriber as _;
    for enabled in [false, true] {
        let recorder = Recorder {
            dump_enabled: enabled,
            events: Arc::default(),
        };
        let events = recorder.events.clone();
        async {
            let (router, _) = fixture().await;
            let response = router
                .clone()
                .oneshot(
                    Request::post("/v1/chat/completions")
                        .header("authorization", "Bearer unused-client-route-key")
                        .body(Body::from("{\"secret\":\"SENSITIVE_CONTENT\","))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let response = router
                .oneshot(
                    Request::post("/v1/unmatched")
                        .body(Body::from("UNCONSUMED_CONTENT"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            to_bytes(response.into_body(), usize::MAX).await.unwrap();
        }
        .with_subscriber(recorder)
        .await;
        let events = events.lock().unwrap();
        let dumps = events
            .iter()
            .filter(|event| {
                event
                    .get("target")
                    .is_some_and(|target| target == "request_dump")
            })
            .collect::<Vec<_>>();
        assert_eq!(!dumps.is_empty(), enabled);
        if enabled {
            let original = dumps
                .iter()
                .filter(|event| {
                    event
                        .get("stage")
                        .is_some_and(|stage| stage == "ingress.request.body")
                })
                .flat_map(|event| STANDARD.decode(event.get("body_base64").unwrap()).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(original, b"{\"secret\":\"SENSITIVE_CONTENT\",");
            let completion = dumps
                .iter()
                .filter(|event| {
                    event
                        .get("stage")
                        .is_some_and(|stage| stage == "ingress.body.finished")
                })
                .map(|event| {
                    serde_json::from_slice::<Value>(
                        &STANDARD.decode(event.get("body_base64").unwrap()).unwrap(),
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            assert!(completion.iter().any(
                |event| event["stage"] == "ingress.request.body" && event["complete"] == false
            ));
            assert!(
                completion
                    .iter()
                    .any(|event| event["stage"] == "ingress.request.body"
                        && event["complete"] == true)
            );
        }
        for event in events.iter().filter(|event| {
            event
                .get("target")
                .is_none_or(|target| target != "request_dump")
        }) {
            assert!(
                !event
                    .values()
                    .any(|value| value.contains("SENSITIVE_CONTENT")
                        || value.contains("unused-client-route-key"))
            );
        }
    }
}
