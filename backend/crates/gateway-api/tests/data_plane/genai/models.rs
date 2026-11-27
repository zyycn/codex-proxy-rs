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

#[tokio::test]
async fn google_api_key_reads_the_same_authorized_catalog() {
    let router = api_router(Arc::new(Execution {
        client: Some(client()),
        ..Default::default()
    }))
    .await;
    for path in ["/v1beta/models", "/v1beta/models/model-a"] {
        let (expected_status, expected) = query(&router, path, true).await;
        let request = Request::get(path)
            .header("x-goog-api-key", KEY)
            .body(Body::empty())
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected_status);
        let actual: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap();
        assert_eq!(actual, expected);
    }
}

fn agent_menu_client() -> AuthenticatedClient {
    use gateway_core::{
        account::ProviderAccountId,
        routing::{AccountModelMetadata, AgentModelListing},
    };
    use std::collections::BTreeMap;
    let source = super::super::authenticated_client_for_provider(KEY, "antigravity");
    let provider = ProviderKind::new("antigravity").unwrap();
    let account = ProviderAccountId::new("acct_api_test").unwrap();
    let private = ProviderAccountId::new("acct_private").unwrap();
    let listed = |order, name: &str| AccountModelMetadata {
        agent_listing: AgentModelListing::Listed {
            order,
            display_name: name.into(),
        },
        ..Default::default()
    };
    let capabilities = ModelCapabilities::new(
        [OperationKind::Generate, OperationKind::CountTokens].into(),
        None,
    )
    .with_generation_protocol(GENAI_PROTOCOL);
    let models = [
        (
            "model-z",
            BTreeMap::from([(account.clone(), listed(0, "Zed"))]),
        ),
        (
            "model-b",
            BTreeMap::from([(account.clone(), listed(1, "Bee"))]),
        ),
        (
            "gemini-3.1-flash-image",
            BTreeMap::from([(account.clone(), listed(2, "Image"))]),
        ),
        (
            "model-hidden",
            BTreeMap::from([
                (
                    account.clone(),
                    AccountModelMetadata {
                        agent_listing: AgentModelListing::Hidden,
                        ..Default::default()
                    },
                ),
                (private, listed(0, "Private Model")),
            ]),
        ),
        (
            "model-fallback",
            BTreeMap::from([(account, AccountModelMetadata::default())]),
        ),
        ("model-plain", BTreeMap::new()),
    ]
    .into_iter()
    .map(|(id, metadata)| {
        ProviderModel::new(
            provider.clone(),
            UpstreamModelId::new(id).unwrap(),
            capabilities.clone().with_account_metadata(metadata),
        )
    })
    .collect();
    let settings = source
        .snapshot()
        .settings()
        .clone()
        .with_model_mappings(BTreeMap::from([("alias-z".into(), "model-z".into())]));
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        settings,
        vec![provider],
        models,
        vec![source.policy().clone()],
    )
    .unwrap();
    super::super::authenticated_client_from_snapshot(KEY, snapshot)
}

fn catalog_client() -> AuthenticatedClient {
    let source = client();
    let settings =
        source
            .snapshot()
            .settings()
            .clone()
            .with_model_mappings(std::collections::BTreeMap::from([(
                "alias-a".into(),
                "model-a".into(),
            )]));
    super::super::authenticated_client_from_snapshot(
        KEY,
        source.snapshot().with_settings(&settings).unwrap(),
    )
}

async fn agent_query(router: &axum::Router, path: &str, user_agent: &str) -> Value {
    let response = router
        .clone()
        .oneshot(
            Request::get(path)
                .header("authorization", format!("Bearer {KEY}"))
                .header("user-agent", user_agent)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap()).unwrap()
}

#[tokio::test]
async fn native_catalog_preserves_metadata_and_aliases_for_every_client() {
    use gateway_core::{operation::RawJsonPayload, routing::PublicModelDescriptor};
    let document = json!({
        "displayName":"Upstream Model", "tagTitle":"New", "tagDescription":"Leaving on Nov 30.",
        "maxTokens":1048576, "maxOutputTokens":65536,
        "future":{"capability":true},
    });
    let payload = RawJsonPayload::new(
        GENAI_PROTOCOL,
        serde_json::to_vec(&document).unwrap().into(),
    )
    .unwrap();
    let router = api_router(Arc::new(Execution {
        client: Some(catalog_client()),
        catalog: Some(
            ["model-a", "model-b", "model-c", "alias-a"]
                .into_iter()
                .map(|id| PublicModelDescriptor::Native {
                    model: PublicModelId::new(id).unwrap(),
                    payload: payload.clone(),
                })
                .collect(),
        ),
        ..Default::default()
    }))
    .await;
    for agent in [
        "ordinary",
        "antigravity/hub/2.34.0",
        "antigravity/cli/1.5.18",
    ] {
        let first = agent_query(&router, "/v1beta/models?pageSize=2", agent).await;
        let second = agent_query(
            &router,
            &format!(
                "/v1beta/models?pageSize=2&pageToken={}",
                first["nextPageToken"].as_str().unwrap()
            ),
            agent,
        )
        .await;
        assert!(second.get("nextPageToken").is_none());
        let entries = first["models"]
            .as_array()
            .unwrap()
            .iter()
            .chain(second["models"].as_array().unwrap());
        assert_eq!(
            entries
                .clone()
                .map(|v| v["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "models/alias-a",
                "models/model-a",
                "models/model-b",
                "models/model-c"
            ]
        );
        for entry in entries {
            let detail = agent_query(
                &router,
                &format!("/v1beta/{}", entry["name"].as_str().unwrap()),
                agent,
            )
            .await;
            assert_eq!(entry, &detail);
            for (key, expected) in document.as_object().unwrap() {
                assert_eq!(&entry[key], expected, "{key}");
            }
            assert_eq!(
                entry["supportedGenerationMethods"],
                json!(["generateContent", "streamGenerateContent", "countTokens"])
            );
            assert!(entry.get("inputTokenLimit").is_none());
        }
    }
}

#[tokio::test]
async fn agent_menus_apply_scoped_visibility_names_and_order_before_pagination() {
    let router = api_router(Arc::new(Execution {
        client: Some(agent_menu_client()),
        ..Default::default()
    }))
    .await;
    for agent in ["antigravity/hub/2.34.0", "antigravity/cli/1.5.17"] {
        let first = agent_query(&router, "/v1beta/models?pageSize=2", agent).await;
        let names = |body: &Value| {
            body["models"]
                .as_array()
                .unwrap()
                .iter()
                .map(|model| (model["name"].clone(), model["displayName"].clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(&first),
            vec![
                (json!("models/alias-z"), json!("alias-z")),
                (json!("models/model-z"), json!("Zed")),
            ]
        );
        let second = agent_query(
            &router,
            &format!(
                "/v1beta/models?pageSize=2&pageToken={}",
                first["nextPageToken"].as_str().unwrap()
            ),
            agent,
        )
        .await;
        assert_eq!(
            names(&second),
            vec![
                (json!("models/model-b"), json!("Bee")),
                (json!("models/gemini-3.1-flash-image"), json!("Image")),
            ]
        );
        let third = agent_query(
            &router,
            &format!(
                "/v1beta/models?pageSize=2&pageToken={}",
                second["nextPageToken"].as_str().unwrap()
            ),
            agent,
        )
        .await;
        assert_eq!(
            names(&third),
            vec![
                (json!("models/model-fallback"), json!("model-fallback")),
                (json!("models/model-plain"), json!("model-plain")),
            ]
        );
        assert!(third.get("nextPageToken").is_none());
        let detail = agent_query(&router, "/v1beta/models/model-hidden", agent).await;
        assert_eq!(detail["name"], "models/model-hidden");
    }
    let (_, ordinary) = query(&router, "/v1beta/models", true).await;
    assert!(
        ordinary["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| model["name"] == "models/model-hidden")
    );
}
