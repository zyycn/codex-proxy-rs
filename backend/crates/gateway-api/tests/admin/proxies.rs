use std::sync::Mutex;

use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use chrono::Utc;
use gateway_admin::{
    model::{MutationContext, Revision, proxies::*},
    ports::{
        proxy::{ProxyProbe, ProxyStore},
        store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
    },
};
use gateway_core::account::OutboundProxy;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::{AdminTestFixture, AdminTestState};

#[derive(Default)]
pub(super) struct MemoryProxies(Mutex<Option<ProxyRecord>>);

fn missing() -> AdminStoreError {
    AdminStoreError::new(AdminStoreErrorKind::NotFound, "proxy", "missing proxy")
}

#[async_trait]
impl ProxyStore for MemoryProxies {
    async fn list(&self, query: ProxyListQuery) -> AdminStoreResult<ProxyPage> {
        let items: Vec<_> = self.0.lock().unwrap().iter().cloned().collect();
        Ok(ProxyPage {
            total: u64::try_from(items.len()).unwrap(),
            items,
            page: query.page,
            page_size: query.page_size.get(),
        })
    }
    async fn get(&self, _: &str) -> AdminStoreResult<ProxyRecord> {
        self.0.lock().unwrap().clone().ok_or_else(missing)
    }
    async fn create(
        &self,
        command: NewProxy,
        _: &MutationContext,
    ) -> AdminStoreResult<ProxyMutation> {
        let record = ProxyRecord {
            id: "proxy_test".to_owned(),
            name: command.name,
            proxy: command.proxy,
            revision: Revision::new(1).unwrap(),
            accounts: vec![],
            last_test: None,
            last_test_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        *self.0.lock().unwrap() = Some(record.clone());
        Ok(ProxyMutation {
            config_revision: record.revision,
            record,
        })
    }
    async fn update(
        &self,
        command: UpdateProxy,
        _: &MutationContext,
    ) -> AdminStoreResult<ProxyMutation> {
        let mut stored = self.0.lock().unwrap();
        let record = stored.as_mut().ok_or_else(missing)?;
        record.name = command.name;
        if let Some(proxy) = command.proxy {
            record.proxy = proxy;
        }
        record.revision = Revision::new(record.revision.get() + 1).unwrap();
        Ok(ProxyMutation {
            config_revision: record.revision,
            record: record.clone(),
        })
    }
    async fn delete(
        &self,
        _: &str,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        self.0.lock().unwrap().take().ok_or_else(missing)?;
        Ok(Revision::new(3).unwrap())
    }
    async fn record_test(
        &self,
        _: &str,
        _: Revision,
        result: ProxyTestResult,
        _: &MutationContext,
    ) -> AdminStoreResult<ProxyRecord> {
        let mut stored = self.0.lock().unwrap();
        let record = stored.as_mut().ok_or_else(missing)?;
        record.last_test = Some(result);
        record.last_test_at = Some(Utc::now());
        Ok(record.clone())
    }
}

pub(super) struct SuccessfulProbe;

#[async_trait]
impl ProxyProbe for SuccessfulProbe {
    async fn test(&self, proxy: &OutboundProxy) -> ProxyTestResult {
        assert_eq!(
            proxy.expose_url(),
            "http://test-user:private-password@proxy.example:8080/"
        );
        ProxyTestResult {
            success: true,
            latency_ms: 15,
            exit_ip: Some("203.0.113.2".parse().unwrap()),
            message: "Connected".to_owned(),
        }
    }
}

async fn request(
    fixture: &AdminTestFixture,
    path: &str,
    body: Option<Value>,
    authenticated: bool,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .header("x-request-id", "req_proxy_tests")
        .uri(path)
        .method(if body.is_some() { "POST" } else { "GET" });
    if authenticated {
        builder = builder.header(header::COOKIE, "cpr_admin_session=valid-session");
    }
    let body = body.map_or_else(Body::empty, |body| Body::from(body.to_string()));
    let response = gateway_api::admin::proxies::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let text = std::str::from_utf8(&body).unwrap();
    assert!(!text.contains("private-password"));
    assert!(!text.contains("test-user"));
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn proxy_routes_save_reload_test_rename_and_delete_without_exposing_credentials() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let (status, created) = request(
        &fixture,
        "/api/admin/proxies/create",
        Some(json!({
            "name": "  Office  ", "proxyUrl": "http://test-user:private-password@proxy.example:8080"
        })),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["data"]["record"]["name"], "Office");
    assert_eq!(created["data"]["record"]["hasAuthentication"], true);
    assert_eq!(
        created["data"]["record"]["endpoint"],
        "http://proxy.example:8080/"
    );
    assert!(created["data"]["record"].get("proxyUrl").is_none());

    let (status, tested) = request(
        &fixture,
        "/api/admin/proxies/test",
        Some(json!({"id": "proxy_test", "revision": 1})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tested["data"]["lastTest"]["exitIp"], "203.0.113.2");
    let (_, listed) = request(
        &fixture,
        "/api/admin/proxies?page=1&pageSize=20",
        None,
        true,
    )
    .await;
    assert_eq!(listed["data"]["items"][0], tested["data"]);

    let (status, _) = request(
        &fixture,
        "/api/admin/proxies/update",
        Some(json!({"id": "proxy_test", "revision": 1, "name": "Renamed"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(
        &fixture,
        "/api/admin/proxies/test",
        Some(json!({"id": "proxy_test", "revision": 1})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = request(
        &fixture,
        "/api/admin/proxies/test",
        Some(json!({"id": "proxy_test", "revision": 2})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(
        &fixture,
        "/api/admin/proxies/delete",
        Some(json!({"id": "proxy_test", "revision": 2})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, listed) = request(&fixture, "/api/admin/proxies", None, true).await;
    assert_eq!(listed["data"]["page"]["total"], 0);
}

#[tokio::test]
async fn proxy_routes_require_auth_and_reject_invalid_input() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    assert_eq!(
        request(&fixture, "/api/admin/proxies", None, false).await.0,
        StatusCode::UNAUTHORIZED
    );
    for action in ["create", "update", "delete", "test"] {
        assert_eq!(
            request(
                &fixture,
                &format!("/api/admin/proxies/{action}"),
                Some(json!({})),
                false
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
    for query in ["page=0", "pageSize=0", "pageSize=201", "unknown=1"] {
        assert_eq!(
            request(&fixture, &format!("/api/admin/proxies?{query}"), None, true)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    for body in [
        json!({"name":" ","proxyUrl":"http://proxy.example:8080"}),
        json!({"name":"Invalid","proxyUrl":""}),
    ] {
        assert_eq!(
            request(&fixture, "/api/admin/proxies/create", Some(body), true)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    let (status, _) = request(
        &fixture,
        "/api/admin/proxies/create",
        Some(json!({"name":"Invalid","proxyUrl":"ftp://test-user:private-password@proxy.example"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}
