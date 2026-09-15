mod usage;

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str::FromStr as _,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Method, Request, StatusCode, header},
};
use chrono::Utc;
use gateway_admin::{
    model::{
        client_usage::ClientUsageKey,
        observability::{
            AttemptMetrics, CostCoverage, CurrencyCost, DashboardObservation, DecimalAmount,
            DiagnosticDimension, DiagnosticObservation, Granularity, OpsErrorPage, OpsErrorQuery,
            ProviderObservation, RequestMetricPoint, RequestMetrics, TimeRange, UsageDetail,
            UsageFilter, UsageListRecord, UsageOverview, UsagePage, UsageQuery, UsageTotals,
        },
        system::{SystemOperationAccepted, SystemUpdateDetail, SystemUpdateStatus, SystemVersion},
    },
    ports::{
        store::{
            AdminStoreError, AdminStoreErrorKind, AdminStoreResult, ClientUsageStore,
            ObservabilityStore,
        },
        system::{SystemOperationError, SystemOperations, SystemUpdateEventStream},
    },
};
use gateway_core::{
    engine::{
        budget::ClientBudgetStatus,
        execution::{ClientAuthenticationError, ClientKeyVerifier},
    },
    policy::{ClientApiKeyId, RateLimits},
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

pub(super) const RAW_KEY: &str = "cpr_live_candidate_must_not_leak";

#[tokio::test]
async fn client_auth_should_issue_restore_and_clear_an_unified_cookie() {
    let (app, _) = client_app().await;
    let login = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/auth/login",
            json!({ "mode": "key", "apiKey": RAW_KEY }),
        ))
        .await
        .expect("client login response");

    assert_eq!(login.status(), StatusCode::OK);
    assert_eq!(login.headers()[header::CACHE_CONTROL], "no-store");
    let set_cookie = login.headers()[header::SET_COOKIE]
        .to_str()
        .expect("client session cookie")
        .to_owned();
    assert!(set_cookie.starts_with("cpr_session=session_"));
    assert!(set_cookie.contains("Path=/;"));
    assert!(set_cookie.contains("; Secure; HttpOnly; SameSite=Lax"));
    assert!(set_cookie.contains("; Max-Age="));
    assert!(set_cookie.contains("; Expires="));
    assert!(!set_cookie.contains(RAW_KEY));
    let cookie = set_cookie
        .split(';')
        .next()
        .expect("cookie pair")
        .to_owned();
    let login_body = response_json(login).await;
    assert_eq!(login_body["data"]["role"], "key");
    assert!(login_body["data"].get("key").is_none());
    assert!(!login_body.to_string().contains(RAW_KEY));

    let status = app
        .clone()
        .oneshot(cookie_request(Method::GET, "/api/auth/status", &cookie))
        .await
        .expect("client status response");
    assert_eq!(status.status(), StatusCode::OK);
    let status_body = response_json(status).await;
    assert_eq!(status_body["data"]["authenticated"], true);
    assert_eq!(status_body["data"]["session"]["role"], "key");

    let logout = app
        .clone()
        .oneshot(cookie_request(Method::POST, "/api/auth/logout", &cookie))
        .await
        .expect("client logout response");
    assert_eq!(logout.status(), StatusCode::OK);
    let cleared = logout.headers()[header::SET_COOKIE]
        .to_str()
        .expect("cleared cookie");
    assert!(cleared.starts_with("cpr_session=;"));
    assert!(cleared.contains("Path=/;"));
    assert!(cleared.contains("Max-Age=0"));
    let status = app
        .oneshot(cookie_request(Method::GET, "/api/auth/status", &cookie))
        .await
        .expect("logged out");
    assert_eq!(response_json(status).await["data"]["authenticated"], false);
}

#[tokio::test]
async fn client_usage_should_require_a_session_and_expose_only_the_session_key_projection() {
    let (app, store) = client_app().await;
    let missing = app
        .clone()
        .oneshot(empty_request(Method::GET, "/api/client/overview"))
        .await
        .expect("missing session response");
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response_json(missing).await["code"], 40101);

    let login = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/auth/login",
            json!({ "mode": "key", "apiKey": RAW_KEY }),
        ))
        .await
        .expect("client login response");
    let cookie = login.headers()[header::SET_COOKIE]
        .to_str()
        .expect("client session cookie")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_owned();

    let response = app
        .oneshot(cookie_request(Method::GET, "/api/client/overview", &cookie))
        .await
        .expect("client usage response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body = response_json(response).await;
    assert_eq!(body["data"]["today"]["totals"]["requestCount"], 7);
    assert_eq!(body["data"]["today"]["billedUsd"], "1.25");
    assert_eq!(
        body["data"]["limits"],
        json!({
            "maxConcurrency": 4,
            "requestsPerMinute": 60
        })
    );
    assert_eq!(body["data"]["key"]["prefix"], "cpr_live_");
    assert!(body["data"]["key"].get("id").is_none());
    assert_no_private_fields(&body);
    assert!(
        store
            .usage_filters()
            .iter()
            .all(|filter| filter.client_api_key_ref.as_deref() == Some("key-42"))
    );
}

#[tokio::test]
async fn invalid_client_login_should_use_a_stable_error_without_echoing_the_key() {
    let store = Arc::new(ClientRouteStore::new());
    let fixture = crate::admin::AdminTestFixture::with_client(
        store.clone(),
        store,
        Arc::new(RejectingVerifier),
        Arc::new(VersionSystem),
    )
    .await;
    let app = crate::openai::api_router_with_admin(fixture.services);

    let response = app
        .oneshot(json_request(
            Method::POST,
            "/api/auth/login",
            json!({ "mode": "key", "apiKey": RAW_KEY }),
        ))
        .await
        .expect("invalid login response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(
        body,
        json!({
            "code": 40102,
            "message": "登录凭据错误",
            "data": null
        })
    );
    assert!(!body.to_string().contains(RAW_KEY));
}

#[tokio::test]
async fn client_version_should_require_a_valid_client_session() {
    let (app, _) = client_app().await;
    for cookie in ["", "cpr_session=unknown-session"] {
        let response = app
            .clone()
            .oneshot(cookie_request(
                Method::GET,
                "/api/client/system/version",
                cookie,
            ))
            .await
            .expect("client version response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response_json(response).await["code"], 40101);
    }
}

#[tokio::test]
async fn client_version_should_restore_from_cookie_and_only_expose_current_version() {
    let (app, _) = client_app().await;
    let login = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/auth/login",
            json!({ "mode": "key", "apiKey": RAW_KEY }),
        ))
        .await
        .expect("client login response");
    let cookie = login.headers()[header::SET_COOKIE]
        .to_str()
        .expect("session cookie")
        .split(';')
        .next()
        .expect("cookie pair");

    let response = app
        .clone()
        .oneshot(cookie_request(
            Method::GET,
            "/api/client/system/version",
            cookie,
        ))
        .await
        .expect("client version response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        response_json(response).await["data"],
        json!({ "version": "3.7.0" })
    );

    let admin_response = app
        .oneshot(cookie_request(
            Method::GET,
            "/api/admin/system/version",
            cookie,
        ))
        .await
        .expect("admin version response");
    assert_eq!(admin_response.status(), StatusCode::FORBIDDEN);
}

pub(super) async fn client_app() -> (axum::Router, Arc<ClientRouteStore>) {
    let store = Arc::new(ClientRouteStore::new());
    let key_id = store.key.lock().expect("client key").id.clone();
    let fixture = crate::admin::AdminTestFixture::with_client(
        store.clone(),
        store.clone(),
        Arc::new(AcceptingVerifier { key_id }),
        Arc::new(VersionSystem),
    )
    .await;
    (
        crate::openai::api_router_with_admin(fixture.services),
        store,
    )
}

pub(super) fn json_request(method: Method, uri: &str, body: Value) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, "https://console.example.test")
        .body(Body::from(body.to_string()))
        .expect("client JSON request");
    request.extensions_mut().insert(ConnectInfo(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)),
        41_000,
    )));
    request
}

pub(super) fn empty_request(method: Method, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("client empty request")
}

pub(super) fn cookie_request(method: Method, uri: &str, cookie: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .expect("client cookie request")
}

pub(super) async fn response_json(response: axum::response::Response) -> Value {
    let body = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read client response body");
    serde_json::from_slice(&body).expect("parse client response JSON")
}

fn assert_no_private_fields(value: &Value) {
    const FORBIDDEN: &[&str] = &[
        "plaintextKey",
        "groups",
        "providerKinds",
        "providerAccounts",
        "providerAccountRef",
        "accountId",
    ];
    match value {
        Value::Object(object) => {
            for name in FORBIDDEN {
                assert!(!object.contains_key(*name), "private field {name} leaked");
            }
            for child in object.values() {
                assert_no_private_fields(child);
            }
        }
        Value::Array(array) => {
            for child in array {
                assert_no_private_fields(child);
            }
        }
        _ => {}
    }
}

struct VersionSystem;

#[async_trait]
impl SystemOperations for VersionSystem {
    async fn version(&self) -> Result<SystemVersion, SystemOperationError> {
        Ok(SystemVersion {
            version: "3.7.0".to_owned(),
            git_sha: "internal-revision".to_owned(),
            build_time: "internal-build-time".to_owned(),
            deployment_mode: "binary".to_owned(),
            update_channel: "release".to_owned(),
            latest_version: "3.8.0".to_owned(),
            has_update: true,
            update_cached: true,
            update_warning: Some("internal-update-warning".to_owned()),
        })
    }

    async fn update_detail(&self, _: bool) -> Result<SystemUpdateDetail, SystemOperationError> {
        unreachable!("client route must not request update details")
    }

    fn update_events(&self) -> SystemUpdateEventStream {
        unreachable!("client route must not subscribe to updates")
    }

    async fn perform_update(
        &self,
        _: Option<String>,
    ) -> Result<SystemOperationAccepted, SystemOperationError> {
        unreachable!("client route must not perform updates")
    }

    async fn update_status(&self) -> Result<SystemUpdateStatus, SystemOperationError> {
        unreachable!("client route must not request update status")
    }

    async fn rollback(&self) -> Result<SystemOperationAccepted, SystemOperationError> {
        unreachable!("client route must not roll back")
    }

    async fn restart(&self) -> Result<SystemOperationAccepted, SystemOperationError> {
        unreachable!("client route must not restart")
    }
}

struct AcceptingVerifier {
    key_id: ClientApiKeyId,
}

impl ClientKeyVerifier for AcceptingVerifier {
    fn verify_client_key(
        &self,
        candidate: &str,
    ) -> Result<ClientApiKeyId, ClientAuthenticationError> {
        if candidate == RAW_KEY {
            Ok(self.key_id.clone())
        } else {
            Err(ClientAuthenticationError::InvalidKey)
        }
    }
}

struct RejectingVerifier;

impl ClientKeyVerifier for RejectingVerifier {
    fn verify_client_key(&self, _: &str) -> Result<ClientApiKeyId, ClientAuthenticationError> {
        Err(ClientAuthenticationError::InvalidKey)
    }
}

pub(super) struct ClientRouteStore {
    pub(super) enabled: AtomicBool,
    pub(super) unavailable: AtomicBool,
    key: Mutex<ClientUsageKey>,
    filters: Mutex<Vec<UsageFilter>>,
    scheduling_metrics: Mutex<RequestMetrics>,
}

impl ClientRouteStore {
    fn new() -> Self {
        let observed_at = chrono::DateTime::parse_from_rfc3339("2026-09-14T12:00:00Z")
            .expect("observed at")
            .with_timezone(&Utc);
        Self {
            enabled: AtomicBool::new(true),
            unavailable: AtomicBool::new(false),
            key: Mutex::new(ClientUsageKey {
                id: ClientApiKeyId::new("key-42").expect("client key id"),
                name: "Automation key".to_owned(),
                label: Some("Production".to_owned()),
                prefix: "cpr_live_".to_owned(),
                limits: RateLimits {
                    max_concurrency: 4,
                    requests_per_minute: 60,
                },
                budget: ClientBudgetStatus::default(),
                last_used_at: Some(observed_at),
                observed_at,
            }),
            filters: Mutex::new(Vec::new()),
            scheduling_metrics: Mutex::new(RequestMetrics::default()),
        }
    }

    fn usage_filters(&self) -> Vec<UsageFilter> {
        self.filters.lock().expect("usage filters").clone()
    }
}

#[async_trait]
impl ClientUsageStore for ClientRouteStore {
    async fn load_client_usage_totals(
        &self,
        key_id: &ClientApiKeyId,
    ) -> AdminStoreResult<UsageTotals> {
        assert_eq!(key_id.as_str(), "key-42");
        Ok(UsageTotals {
            request_count: 70,
            input_tokens: 8_000,
            cached_tokens: 4_000,
            total_tokens: 10_000,
            billing_usd: Some(DecimalAmount::from_str("12.5").expect("lifetime cost")),
        })
    }

    async fn load_client_usage_key(
        &self,
        id: &ClientApiKeyId,
    ) -> AdminStoreResult<Option<ClientUsageKey>> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(unavailable("key status"));
        }
        Ok(Some(self.key.lock().expect("client key").clone())
            .filter(|key| key.id == *id && self.enabled.load(Ordering::SeqCst)))
    }
}

#[async_trait]
impl ObservabilityStore for ClientRouteStore {
    async fn dashboard_summary(
        &self,
        _: TimeRange,
        _: chrono::DateTime<Utc>,
    ) -> AdminStoreResult<DashboardObservation> {
        Err(unavailable("dashboard"))
    }

    async fn dashboard_trend(&self, _: TimeRange) -> AdminStoreResult<Vec<RequestMetricPoint>> {
        Err(unavailable("dashboard trend"))
    }

    async fn usage_trend(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> AdminStoreResult<Vec<RequestMetricPoint>> {
        self.filters.lock().expect("usage filters").push(filter);
        Ok(vec![RequestMetricPoint {
            bucket_start: range.start,
            granularity: Granularity::Hour,
            metrics: RequestMetrics {
                request_count: 7,
                success_count: 6,
                failure_count: 1,
                total_tokens: 1_000,
                ..self
                    .scheduling_metrics
                    .lock()
                    .expect("scheduling metrics")
                    .clone()
            },
            cost_coverage: CostCoverage::default(),
            costs: vec![usd_cost("1.25")],
        }])
    }

    fn usage_calculated_billing_facts(
        &self,
        _: TimeRange,
        _: UsageFilter,
    ) -> gateway_admin::ports::store::UsageCalculatedBillingStream<'_> {
        Box::pin(futures::stream::empty())
    }

    async fn list_usage_records(&self, query: UsageQuery) -> AdminStoreResult<UsagePage> {
        self.filters
            .lock()
            .expect("usage filters")
            .push(query.filter);
        Ok(UsagePage {
            items: vec![usage_record(query.range.start)],
            current_page: query.current_page,
            page_size: query.page_size.get(),
            total: 1,
        })
    }

    async fn usage_record_detail(&self, _: &str) -> AdminStoreResult<UsageDetail> {
        Err(unavailable("usage detail"))
    }

    async fn usage_summary(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> AdminStoreResult<UsageOverview> {
        self.filters.lock().expect("usage filters").push(filter);
        Ok(UsageOverview {
            range,
            requests: RequestMetrics {
                request_count: 7,
                success_count: 6,
                failure_count: 1,
                input_tokens: 800,
                output_tokens: 200,
                cached_tokens: 400,
                total_tokens: 1_000,
                ..self
                    .scheduling_metrics
                    .lock()
                    .expect("scheduling metrics")
                    .clone()
            },
            attempts: AttemptMetrics {
                attempt_count: 7,
                cost_coverage: CostCoverage {
                    provider_reported_count: 7,
                    ..CostCoverage::default()
                },
                costs: vec![usd_cost("1.25")],
                ..AttemptMetrics::default()
            },
            // Presenter 必须丢弃 Provider / Account 侧内部维度。
            providers: vec![ProviderObservation {
                provider_kind: "internal-provider".to_owned(),
                request_count: 7,
                attempt_count: 7,
                failure_count: 1,
                total_tokens: 1_000,
            }],
        })
    }

    async fn usage_diagnostics(
        &self,
        _: TimeRange,
        _: UsageFilter,
        _: DiagnosticDimension,
    ) -> AdminStoreResult<Vec<DiagnosticObservation>> {
        Err(unavailable("usage diagnostics"))
    }

    async fn list_ops_errors(&self, _: OpsErrorQuery) -> AdminStoreResult<OpsErrorPage> {
        Err(unavailable("ops errors"))
    }
}

fn usage_record(started_at: chrono::DateTime<Utc>) -> UsageListRecord {
    UsageListRecord {
        id: "request-42".to_owned(),
        endpoint: "/v1/responses".to_owned(),
        client_transport: "ws".to_owned(),
        requested_model_id: Some("gpt-5".to_owned()),
        provider_kind: Some("internal-provider".to_owned()),
        provider_account_ref: Some("account-secret".to_owned()),
        provider_account_name: Some("Internal account".to_owned()),
        provider_account_email: Some("internal@example.test".to_owned()),
        provider_account_authentication_kind: Some("oauth".to_owned()),
        upstream_model_id: Some("gpt-5".to_owned()),
        upstream_transport: Some("ws".to_owned()),
        service_tier: Some("default".to_owned()),
        input_tokens: Some(800),
        output_tokens: Some(200),
        cached_tokens: Some(400),
        cache_write_tokens: Some(0),
        reasoning_tokens: Some(50),
        image_input_tokens: Some(0),
        image_output_tokens: Some(0),
        total_tokens: Some(1_000),
        cost_source: "provider_reported".to_owned(),
        cost_amount: Some(DecimalAmount::from_str("1.25").expect("record cost")),
        cost_currency: Some("USD".to_owned()),
        billing: None,
        transport_decision_wait_ms: Some(5),
        connect_ms: Some(20),
        headers_ms: Some(50),
        first_event_ms: Some(100),
        first_reasoning_ms: Some(120),
        first_text_ms: Some(150),
        first_token_ms: Some(160),
        provider_processing_ms: Some(900),
        latency_ms: Some(1_234),
        admission_decision_ms: Some(2),
        account_selection_wait_ms: Some(3),
        capacity_used_slots: Some(1),
        capacity_total_slots: Some(4),
        client_ip: Some("203.0.113.10".to_owned()),
        user_agent: Some("codex-cli/test".to_owned()),
        reasoning_effort: Some("high".to_owned()),
        reasoning_preset: None,
        subagent_kind: None,
        compact: false,
        started_at,
    }
}

fn usd_cost(amount: &str) -> CurrencyCost {
    CurrencyCost {
        currency: "USD".to_owned(),
        amount: DecimalAmount::from_str(amount).expect("USD cost"),
    }
}

fn unavailable(resource: &'static str) -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        resource,
        "test unavailable",
    )
}
