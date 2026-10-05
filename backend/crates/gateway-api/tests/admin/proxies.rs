//! 验证代理管理 HTTP 接口的位置字段、部分更新与账号操作

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
use gateway_core::account::{OutboundProxy, ProviderAccountId};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::{AdminTestFixture, AdminTestState};

#[tokio::test]
async fn proxy_location_round_trips_preserves_omitted_and_clears_null() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let location =
        json!({"country":"JP", "region":"Tokyo", "city":"Tokyo", "timezone":"Asia/Tokyo"});
    let (status, created) = request(
        &fixture,
        "/api/admin/proxies/create",
        Some(json!({
            "name":"Tokyo", "proxyUrl":"http://proxy.example:8080", "location":location
        })),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["data"]["record"]["location"], location);
    let (_, listed) = request(&fixture, "/api/admin/proxies", None, true).await;
    assert_eq!(listed["data"]["items"][0]["location"], location);
    for (revision, change, expected) in [
        (1, None, location.clone()),
        (2, Some(Value::Null), Value::Null),
        (3, Some(location.clone()), location.clone()),
    ] {
        let mut body = json!({"id":"proxy_test", "revision":revision, "name":"Renamed"});
        if let Some(change) = change {
            body["location"] = change;
        }
        let (status, changed) =
            request(&fixture, "/api/admin/proxies/update", Some(body), true).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(changed["data"]["record"]["location"], expected);
    }
}

#[tokio::test]
async fn proxy_location_rejects_invalid_and_incomplete_input() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    for (field, value) in [
        ("country", json!("jp")),
        ("city", json!(" ")),
        ("region", json!("Tokyo\n")),
        ("city", json!("x".repeat(129))),
        ("timezone", json!("Asia/Typo")),
    ] {
        let mut invalid =
            json!({"country":"JP", "region":"Tokyo", "city":"Tokyo", "timezone":"Asia/Tokyo"});
        invalid[field] = value;
        let (status, _) = request(&fixture, "/api/admin/proxies/create", Some(json!({"name":"Invalid", "proxyUrl":"http://proxy.example:8080", "location":invalid})), true).await;
        assert!(status.is_client_error(), "invalid {field} accepted");
    }
    let (status, _) = request(&fixture, "/api/admin/proxies/create", Some(json!({"name":"Incomplete", "proxyUrl":"http://proxy.example:8080", "location":{"country":"JP"}})), true).await;
    assert!(status.is_client_error());
    let (status, created) = request(
        &fixture,
        "/api/admin/proxies/create",
        Some(json!({"name":"Legacy", "proxyUrl":"http://proxy.example:8080"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(created["data"]["record"]["location"].is_null());
}

#[derive(Default)]
pub(super) struct MemoryProxies(Mutex<Option<ProxyRecord>>);

fn missing() -> AdminStoreError {
    AdminStoreError::new(AdminStoreErrorKind::NotFound, "proxy", "missing proxy")
}

#[async_trait]
impl ProxyStore for MemoryProxies {
    async fn remove_account(
        &self,
        proxy_id: &str,
        account_id: &ProviderAccountId,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        if proxy_id != "proxy_test" || account_id.as_str() != "acct_linked" {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::Conflict,
                "proxy account",
                "changed binding",
            ));
        }
        Ok(Revision::new(4).unwrap())
    }

    async fn reserve_import(
        &self,
        _: &str,
    ) -> AdminStoreResult<gateway_admin::ports::proxy::ProxyImportReservation> {
        Err(missing())
    }
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
    async fn list_accounts(
        &self,
        query: ProxyAccountListQuery,
    ) -> AdminStoreResult<ProxyAccountPage> {
        if !self
            .0
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|record| record.id == query.proxy_id)
        {
            return Err(missing());
        }
        Ok(ProxyAccountPage {
            items: vec![ProxyAccountRef {
                id: "acct_linked".to_owned(),
                name: "工作账号".to_owned(),
                email: Some("work@example.invalid".to_owned()),
                provider_kind: "openai".to_owned(),
                authentication_kind: "oauth".to_owned(),
                plan_type: Some("plus".to_owned()),
                plan_type_display: None,
                groups: vec![],
                enabled: true,
            }],
            total: 21,
            page: query.page,
            page_size: query.page_size.get(),
        })
    }
    async fn create(
        &self,
        command: NewProxy,
        _: &MutationContext,
    ) -> AdminStoreResult<ProxyMutation> {
        let mut record = ProxyRecord {
            auto_location: command.auto_location,
            detected_location: None,
            location: command.location,
            id: "proxy_test".to_owned(),
            name: command.name,
            proxy: command.proxy,
            revision: Revision::new(1).unwrap(),
            account_count: 0,
            last_test: None,
            last_test_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        if let Some(result) = command.test {
            record.detected_location = record.detected_location_after_test(&result);
            record.last_test = Some(result);
            record.last_test_at = Some(Utc::now());
        }
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
        if let Some(auto_location) = command.auto_location {
            if auto_location != record.auto_location {
                record.detected_location = None;
            }
            record.auto_location = auto_location;
        }
        if let Some(location) = command.location {
            record.location = location;
        }
        if let Some(proxy) = command.proxy {
            if record.proxy != proxy {
                record.detected_location = None;
            }
            record.proxy = proxy;
        }
        if let Some(result) = command.test {
            record.detected_location = record.detected_location_after_test(&result);
            record.last_test = Some(result);
            record.last_test_at = Some(Utc::now());
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
    ) -> AdminStoreResult<ProxyMutation> {
        let mut stored = self.0.lock().unwrap();
        let record = stored.as_mut().ok_or_else(missing)?;
        record.detected_location = record.detected_location_after_test(&result);
        record.last_test = Some(result);
        record.last_test_at = Some(Utc::now());
        if record.auto_location {
            record.revision = Revision::new(record.revision.get() + 1).unwrap();
        }
        Ok(ProxyMutation {
            config_revision: record.revision,
            record: record.clone(),
        })
    }
}

pub(super) struct SuccessfulProbe;

#[async_trait]
impl ProxyProbe for SuccessfulProbe {
    async fn test(&self, proxy: &OutboundProxy, detect_location: bool) -> ProxyTestResult {
        assert_eq!(
            proxy.expose_url(),
            "http://test-user:private-password@proxy.example:8080/"
        );
        ProxyTestResult {
            location: if detect_location {
                ProxyLocationDetection::Detected { location: serde_json::from_value(json!({"country":"JP","region":"Tokyo","city":"Tokyo","timezone":"Asia/Tokyo"})).unwrap() }
            } else {
                ProxyLocationDetection::NotRequested
            },
            success: true,
            latency_ms: 15,
            exit_ip: Some("203.0.113.2".parse().unwrap()),
            exit_ipv4: Some("203.0.113.2".parse().unwrap()),
            exit_ipv6: None,
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
        builder = builder.header(header::COOKIE, "cpr_session=valid-session");
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
    assert_eq!(created["data"]["record"]["accountCount"], 0);
    assert!(created["data"]["record"].get("accounts").is_none());

    let (status, linked) = request(
        &fixture,
        "/api/admin/proxies/accounts?proxyId=proxy_test&page=2&pageSize=20&search=work",
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        linked["data"]["page"],
        json!({"page":2,"pageSize":20,"total":21,"totalPages":2})
    );
    assert_eq!(
        linked["data"]["items"],
        json!([{
            "id":"acct_linked", "name":"工作账号", "email":"work@example.invalid",
            "provider":"openai", "enabled":true, "authenticationKind":"oauth",
            "planType":"plus", "planTypeDisplay":"Plus", "groups":[]
        }])
    );

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
async fn proxy_probe_checks_unsaved_address_without_creating_or_changing_records() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let draft = json!({
        "proxyUrl": "http://test-user:private-password@proxy.example:8080"
    });
    let (status, probed) = request(
        &fixture,
        "/api/admin/proxies/probe",
        Some(draft.clone()),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        probed["data"],
        json!({
            "location": {"status": "notRequested"},
            "success": true,
            "latencyMs": 15,
            "exitIp": "203.0.113.2",
            "exitIpv4": "203.0.113.2",
            "exitIpv6": null,
            "message": "Connected"
        })
    );
    let (_, listed) = request(&fixture, "/api/admin/proxies", None, true).await;
    assert_eq!(listed["data"]["page"]["total"], 0);

    let (_, created) = request(
        &fixture,
        "/api/admin/proxies/create",
        Some(json!({"name": "Saved", "proxyUrl": "http://saved.example:8080"})),
        true,
    )
    .await;
    let (status, _) = request(&fixture, "/api/admin/proxies/probe", Some(draft), true).await;
    assert_eq!(status, StatusCode::OK);
    let (_, listed) = request(&fixture, "/api/admin/proxies", None, true).await;
    assert_eq!(listed["data"]["items"][0], created["data"]["record"]);
}

#[tokio::test]
async fn proxy_probe_rejects_empty_or_invalid_addresses() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    for (body, expected) in [
        (json!({"proxyUrl": ""}), StatusCode::BAD_REQUEST),
        (json!({"proxyUrl": null}), StatusCode::UNPROCESSABLE_ENTITY),
        (json!({}), StatusCode::UNPROCESSABLE_ENTITY),
        (
            json!({"proxyUrl": "ftp://test-user:private-password@proxy.example"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"proxyUrl": "http://proxy.example:8080", "name": "Not saved"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        assert_eq!(
            request(&fixture, "/api/admin/proxies/probe", Some(body), true)
                .await
                .0,
            expected
        );
    }
}

#[tokio::test]
async fn proxy_routes_require_auth_and_reject_invalid_input() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    assert_eq!(
        request(&fixture, "/api/admin/proxies", None, false).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &fixture,
            "/api/admin/proxies/accounts?proxyId=proxy_test",
            None,
            false
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    for query in [
        "",
        "proxyId=",
        "proxyId=proxy_test&page=0",
        "proxyId=proxy_test&pageSize=0",
        "proxyId=proxy_test&pageSize=201",
        "proxyId=proxy_test&search=%00",
        "proxyId=proxy_test&unknown=1",
    ] {
        assert_eq!(
            request(
                &fixture,
                &format!("/api/admin/proxies/accounts?{query}"),
                None,
                true
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        request(
            &fixture,
            "/api/admin/proxies/accounts?proxyId=missing",
            None,
            true
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    for action in [
        "create",
        "update",
        "delete",
        "test",
        "probe",
        "accounts/remove",
    ] {
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

#[tokio::test]
async fn proxy_account_removal_validates_binding_and_input() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    for (body, expected) in [
        (
            json!({"proxyId":"", "accountId":"acct_linked"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"proxyId":"proxy_test", "accountId":""}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"proxyId":"proxy_test", "accountId":"invalid"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"proxyId":"proxy_test", "accountId":"acct_linked", "enabled":false}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        assert_eq!(
            request(
                &fixture,
                "/api/admin/proxies/accounts/remove",
                Some(body),
                true
            )
            .await
            .0,
            expected
        );
    }
    let (status, result) = request(
        &fixture,
        "/api/admin/proxies/accounts/remove",
        Some(json!({"proxyId":"proxy_test", "accountId":"acct_linked"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["data"], json!({"configRevision":4}));
    let (status, result) = request(
        &fixture,
        "/api/admin/proxies/accounts/remove",
        Some(json!({"proxyId":"proxy_previous", "accountId":"acct_linked"})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(result["message"], "账号的代理绑定已变化，请刷新后重试");
}

#[tokio::test]
async fn auto_location_detects_on_enable_preserves_manual_and_refreshes_only_on_explicit_test() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let manual = json!({"country":"US", "region":"California", "city":"Los Angeles", "timezone":"America/Los_Angeles"});
    let (status, created) = request(&fixture, "/api/admin/proxies/create", Some(json!({
        "name":"Automatic", "proxyUrl":"http://test-user:private-password@proxy.example:8080", "autoLocation":true, "location":manual
    })), true).await;
    assert_eq!(status, StatusCode::CREATED);
    let record = &created["data"]["record"];
    assert_eq!(record["autoLocation"], true);
    assert_eq!(
        record["detectedLocation"]["location"]["timezone"],
        "Asia/Tokyo"
    );
    assert_eq!(record["location"], manual);
    let (_, renamed) = request(
        &fixture,
        "/api/admin/proxies/update",
        Some(json!({
            "id":"proxy_test", "revision":1, "name":"Renamed", "autoLocation":true
        })),
        true,
    )
    .await;
    assert_eq!(
        renamed["data"]["record"]["detectedLocation"],
        record["detectedLocation"]
    );
    assert_eq!(
        renamed["data"]["record"]["lastTestAt"],
        record["lastTestAt"]
    );
    let (status, tested) = request(
        &fixture,
        "/api/admin/proxies/test",
        Some(json!({"id":"proxy_test", "revision":2})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tested["data"]["revision"], 3);
    assert_eq!(tested["data"]["lastTest"]["location"]["status"], "detected");
    let (_, disabled) = request(
        &fixture,
        "/api/admin/proxies/update",
        Some(json!({
            "id":"proxy_test", "revision":3, "name":"Manual", "autoLocation":false
        })),
        true,
    )
    .await;
    assert_eq!(disabled["data"]["record"]["autoLocation"], false);
    assert_eq!(disabled["data"]["record"]["location"], manual);
    assert!(disabled["data"]["record"]["detectedLocation"].is_null());
}

#[tokio::test]
async fn manual_proxy_can_detect_location_once_without_enabling_auto_location() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let manual = json!({"country":"US", "region":"California", "city":"Los Angeles", "timezone":"America/Los_Angeles"});
    let (status, created) = request(&fixture, "/api/admin/proxies/create", Some(json!({
        "name":"Manual", "proxyUrl":"http://test-user:private-password@proxy.example:8080", "location":manual
    })), true).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["data"]["record"]["autoLocation"], false);

    let (status, tested) = request(
        &fixture,
        "/api/admin/proxies/test",
        Some(json!({"id":"proxy_test", "revision":1, "detectLocation":true})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let record = &tested["data"];
    assert_eq!(record["revision"], 1);
    assert_eq!(record["autoLocation"], false);
    assert_eq!(record["location"], manual);
    assert_eq!(
        record["detectedLocation"]["location"]["timezone"],
        "Asia/Tokyo"
    );
    assert_eq!(record["lastTest"]["location"]["status"], "detected");
    assert_eq!(
        record["lastTest"]["location"]["location"]["timezone"],
        "Asia/Tokyo"
    );
}
