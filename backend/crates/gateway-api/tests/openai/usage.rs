//! 验证当前 Key 预算查询的身份隔离与只读行为

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    Router,
    http::{Method, StatusCode, header},
    response::Response,
};
use chrono::{Duration, Utc};
use futures::future::BoxFuture;
use gateway_core::{
    engine::budget::{ClientBudgetLimits, ClientBudgetStatus},
    engine::{
        authentication::ClientAuthenticationRequest,
        execution::{
            AuthenticatedClient, ClientAuthenticationError, ExecutionService, StartExecution,
            StartProviderExecution, StartedExecution,
        },
    },
    error::{GatewayError, GatewayErrorKind},
    policy::ClientApiKeyId,
    routing::PublicModelId,
};
use serde_json::json;
use tower::ServiceExt as _;

use crate::{
    admin::AdminTestFixture,
    support::{RAW_KEY, empty_request, key_fixture, response_json},
};

const KEY: &str = RAW_KEY;

async fn fixture() -> (AdminTestFixture, Router) {
    let fixture = key_fixture().await;
    let id = ClientApiKeyId::new("key-42").unwrap();
    let mut key = fixture
        .services
        .client_keys()
        .reveal(&id)
        .await
        .unwrap()
        .record;
    key.id = id;
    key.label = Some("private-usage-sentinel".to_owned());
    key.budget = ClientBudgetStatus {
        limits: ClientBudgetLimits {
            daily_usd: "1".parse().unwrap(),
            weekly_usd: "5".parse().unwrap(),
        },
        daily_used_usd: "0.6400000001".parse().unwrap(),
        weekly_used_usd: "2.35".parse().unwrap(),
        daily_resets_at: Some((Utc::now() + Duration::days(1)).into()),
        weekly_resets_at: Some((Utc::now() + Duration::days(7)).into()),
    };
    *fixture.client_key.lock().unwrap() = Some(key);
    let app = super::api_router_with_admin_and_client(fixture.services.clone(), KEY, "key-42");
    (fixture, app)
}

async fn query(app: &Router, path: &str, authorization: Option<&str>) -> Response {
    let mut request = empty_request(Method::GET, path);
    if let Some(value) = authorization {
        request
            .headers_mut()
            .insert(header::AUTHORIZATION, value.parse().unwrap());
    }
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    response
}

#[tokio::test]
async fn usage_returns_current_key_budget_without_exposing_private_data_or_writing_usage() {
    let (fixture, app) = fixture().await;
    let before = fixture.client_key.lock().unwrap().clone().unwrap();
    let response = query(&app, "/v1/usage", Some(&format!("Bearer {KEY}"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(
        body,
        json!({
            "unit": "USD",
            "daily": {
                "total": "1", "used": "0.6400000001", "remaining": "0.3599999999",
                "resetsAt": before.budget.daily_resets_at.map(chrono::DateTime::<Utc>::from),
            },
            "weekly": {
                "total": "5", "used": "2.35", "remaining": "2.65",
                "resetsAt": before.budget.weekly_resets_at.map(chrono::DateTime::<Utc>::from),
            },
        })
    );
    assert!(!body.to_string().contains("private-usage-sentinel"));
    assert_eq!(fixture.client_key.lock().unwrap().as_ref(), Some(&before));
    assert!(fixture.observations.lock().unwrap().summaries.is_empty());
}

struct FrontendUsageAuthentication {
    client: AuthenticatedClient,
    authentication_calls: AtomicUsize,
    verification_calls: AtomicUsize,
    middleware: Arc<crate::openai::middleware::RequestMiddleware>,
}

impl ExecutionService for FrontendUsageAuthentication {
    fn middleware_plan(
        &self,
        _: &gateway_core::engine::execution::PreparedRootExecution,
    ) -> Option<gateway_core::engine::middleware::FrozenMiddlewarePlan> {
        Some(self.middleware.frozen())
    }

    fn authenticate(&self, _: &str) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        Err(ClientAuthenticationError::InvalidKey)
    }

    fn authenticate_request(
        &self,
        _: ClientAuthenticationRequest,
    ) -> BoxFuture<'_, Result<AuthenticatedClient, ClientAuthenticationError>> {
        self.authentication_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(ClientAuthenticationError::InvalidKey) })
    }

    fn verify_request(
        &self,
        request: ClientAuthenticationRequest,
    ) -> BoxFuture<'_, Result<AuthenticatedClient, ClientAuthenticationError>> {
        assert_eq!(request.authorization(), "External controlled-fixture");
        self.verification_calls.fetch_add(1, Ordering::SeqCst);
        let client = self.client.clone();
        Box::pin(async move { Ok(client) })
    }

    fn public_models(&self, _: &AuthenticatedClient) -> Vec<PublicModelId> {
        Vec::new()
    }

    fn contains_public_model(&self, _: &AuthenticatedClient, _: &PublicModelId) -> bool {
        false
    }

    fn start(&self, _: StartExecution) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async {
            Err(GatewayError::new(
                GatewayErrorKind::Internal,
                "usage route must not start an execution",
            ))
        })
    }

    fn start_provider_endpoint(
        &self,
        _: StartProviderExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async {
            Err(GatewayError::new(
                GatewayErrorKind::Internal,
                "usage route must not start a provider execution",
            ))
        })
    }
}

#[tokio::test]
async fn usage_uses_the_entry_authentication_identity_without_recording_request_usage() {
    let fixture = key_fixture().await;
    let id = ClientApiKeyId::new("key_api_test").unwrap();
    let key = fixture
        .services
        .client_keys()
        .reveal(&id)
        .await
        .unwrap()
        .record;
    *fixture.client_key.lock().unwrap() = Some(key);
    let execution = Arc::new(FrontendUsageAuthentication {
        client: super::authenticated_client("unused-native-key"),
        authentication_calls: AtomicUsize::new(0),
        verification_calls: AtomicUsize::new(0),
        middleware: Arc::new(crate::openai::middleware::RequestMiddleware::default()),
    });
    let app =
        super::api_router_with_admin_and_execution(fixture.services.clone(), execution.clone());

    let response = query(&app, "/v1/usage", Some("External controlled-fixture")).await;
    assert_eq!(response.headers()["x-request-middleware"], "applied");
    assert_eq!(
        *execution.middleware.endpoints.lock().unwrap(),
        ["/v1/usage"]
    );

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(execution.verification_calls.load(Ordering::SeqCst), 1);
    assert_eq!(execution.authentication_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn usage_remains_readable_when_exhausted_and_never_returns_negative_remaining() {
    let (fixture, app) = fixture().await;
    {
        let mut key = fixture.client_key.lock().unwrap();
        let budget = &mut key.as_mut().unwrap().budget;
        budget.daily_used_usd = "1".parse().unwrap();
        budget.weekly_used_usd = "5.75".parse().unwrap();
    }
    let response = query(&app, "/v1/usage", Some(&format!("Bearer {KEY}"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["daily"]["remaining"], "0");
    assert_eq!(body["weekly"]["remaining"], "0");
    assert_eq!(body["weekly"]["used"], "5.75");
}

#[tokio::test]
async fn usage_distinguishes_unlimited_and_unused_windows_from_exhausted_budgets() {
    let (fixture, app) = fixture().await;
    for (daily, weekly) in [("0", "0"), ("1", "0"), ("0", "5")] {
        fixture.client_key.lock().unwrap().as_mut().unwrap().budget = ClientBudgetStatus {
            limits: ClientBudgetLimits {
                daily_usd: daily.parse().unwrap(),
                weekly_usd: weekly.parse().unwrap(),
            },
            ..Default::default()
        };
        let response = query(&app, "/v1/usage", Some(&format!("Bearer {KEY}"))).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        for (period, limit) in [("daily", daily), ("weekly", weekly)] {
            assert_eq!(
                body[period],
                json!({
                    "total": (limit != "0").then_some(limit),
                    "used": "0",
                    "remaining": (limit != "0").then_some(limit),
                    "resetsAt": null,
                })
            );
        }
    }
}

#[tokio::test]
async fn usage_rejects_missing_invalid_disabled_deleted_and_other_keys() {
    let (fixture, app) = fixture().await;
    for authorization in [None, Some("Basic invalid"), Some("Bearer unknown-key")] {
        let response = query(&app, "/v1/usage", authorization).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response_json(response).await.get("error").is_some());
    }
    let original = fixture.client_key.lock().unwrap().clone().unwrap();
    for mode in ["disabled", "deleted", "other"] {
        {
            let mut key = fixture.client_key.lock().unwrap();
            *key = Some(original.clone());
            match mode {
                "disabled" => key.as_mut().unwrap().enabled = false,
                "deleted" => *key = None,
                _ => key.as_mut().unwrap().id = ClientApiKeyId::new("other-key").unwrap(),
            }
        }
        let response = query(&app, "/v1/usage", Some(&format!("Bearer {KEY}"))).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{mode}");
    }
}

#[tokio::test]
async fn usage_does_not_accept_sessions_admin_keys_or_caller_selected_scope() {
    let (fixture, app) = fixture().await;
    fixture.auth.insert_session("valid-admin");
    for (name, value) in [
        (header::COOKIE.as_str(), "cpr_session=valid-admin"),
        ("x-api-key", KEY),
    ] {
        let mut request = empty_request(Method::GET, "/v1/usage");
        request.headers_mut().insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    for suffix in [
        "?keyId=other",
        "?clientApiKeyRef=other",
        "?startTime=2026-01-01",
        "?api_key=test",
    ] {
        let response = query(
            &app,
            &format!("/v1/usage{suffix}"),
            Some(&format!("Bearer {KEY}")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(response).await["error"]["code"],
            "invalid_usage_query"
        );
    }
}
