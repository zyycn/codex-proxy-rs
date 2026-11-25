//! 原生模型目录的分页、详情、鉴权与能力投影

use super::*;
use gateway_core::{
    operation::OperationKind,
    routing::{
        ConfigRevision, ModelCapabilities, ProviderKind, ProviderModel, RuntimeSnapshot,
        UpstreamModelId,
    },
};

fn client() -> AuthenticatedClient {
    let source = authenticated_client(KEY);
    let caps = ModelCapabilities::new(
        [OperationKind::Generate, OperationKind::CountTokens].into(),
        None,
    )
    .with_generation_protocol(GENAI_PROTOCOL);
    let provider = ProviderKind::new("openai").unwrap();
    let models = ["model-a", "model-b", "model-c", "unreachable/name"].map(|id| {
        ProviderModel::new(
            provider.clone(),
            UpstreamModelId::new(id).unwrap(),
            caps.clone(),
        )
    });
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        source.snapshot().settings().clone(),
        vec![provider],
        models.into(),
        vec![source.policy().clone()],
    )
    .unwrap();
    super::super::authenticated_client_from_snapshot(KEY, snapshot)
}

async fn query(router: &axum::Router, path: &str, authorized: bool) -> (StatusCode, Value) {
    let mut request = Request::get(path);
    if authorized {
        request = request.header("authorization", format!("Bearer {KEY}"));
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let value =
        serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
    (status, value)
}

#[tokio::test]
async fn list_pages_and_details_expose_only_supported_native_models() {
    let execution = Arc::new(Execution {
        client: Some(client()),
        ..Default::default()
    });
    let router = api_router(execution.clone()).await;
    let (status, first) = query(&router, "/v1beta/models?pageSize=2", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["models"].as_array().unwrap().len(), 2);
    assert_eq!(first["models"][0]["name"], "models/model-a");
    assert_eq!(
        first["models"][0]["supportedGenerationMethods"],
        json!(["generateContent", "streamGenerateContent", "countTokens"])
    );
    assert!(first["models"][0].get("inputTokenLimit").is_none());
    let token = first["nextPageToken"].as_str().unwrap();
    let (_, second) = query(
        &router,
        &format!("/v1beta/models?pageSize=2&pageToken={token}"),
        true,
    )
    .await;
    assert_eq!(
        second["models"],
        json!([{"name":"models/model-c","displayName":"model-c","supportedGenerationMethods":["generateContent","streamGenerateContent","countTokens"]}])
    );
    assert!(second.get("nextPageToken").is_none());
    let (status, _) = query(
        &router,
        &format!("/v1beta/models?pageSize=1&pageToken={token}"),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, detail) = query(&router, "/v1beta/models/model-a", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail, first["models"][0]);
    assert!(execution.seen.lock().unwrap().is_none());
    let (_, empty) = query(
        &api_router(Arc::new(Execution::default())).await,
        "/v1beta/models",
        true,
    )
    .await;
    assert_eq!(empty, json!({"models":[]}));
}

#[tokio::test]
async fn directory_errors_follow_google_rpc_and_require_authentication() {
    let router = api_router(Arc::new(Execution {
        client: Some(client()),
        ..Default::default()
    }))
    .await;
    for (path, authorized, status) in [
        ("/v1beta/models", false, StatusCode::UNAUTHORIZED),
        ("/v1beta/models/model-a", false, StatusCode::UNAUTHORIZED),
        ("/v1beta/models/unknown", true, StatusCode::NOT_FOUND),
        ("/v1beta/models?pageSize=-1", true, StatusCode::BAD_REQUEST),
        (
            "/v1beta/models?pageToken=bad",
            true,
            StatusCode::BAD_REQUEST,
        ),
        ("/v1beta/models?pageSize=abc", true, StatusCode::BAD_REQUEST),
    ] {
        let (actual, body) = query(&router, path, authorized).await;
        assert_eq!(actual, status, "{path}");
        assert_eq!(body["error"]["code"], status.as_u16(), "{path}");
    }
}
