//! 验证账号管理接口的身份要求、查询校验与服务调用边界

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use gateway_api::admin;
use tower::ServiceExt as _;

use super::super::{AdminTestFixture, AdminTestState};

#[tokio::test]
async fn account_cooldown_returns_an_instant_and_server_formatted_recovery() {
    use chrono::{DateTime, Duration, Utc};
    use gateway_core::account::{
        AccountCooldown, AccountCooldownKind, AccountStatusFacts, CredentialState, QuotaState,
    };

    for zone in ["Asia/Shanghai", "UTC", "Asia/Kathmandu", "America/New_York"] {
        let zone = zone
            .parse::<gateway_core::time::DeploymentTimeZone>()
            .unwrap();
        let fixture = AdminTestFixture::with_timezone(zone).await;
        fixture.auth.insert_session("valid-session");
        for (seconds, kind, expected) in [
            (
                Some(1200),
                AccountCooldownKind::RateLimit,
                Some("剩余 20 分钟"),
            ),
            (
                Some(7200),
                AccountCooldownKind::CapacityFreeze,
                Some("剩余 2 小时"),
            ),
            (
                Some(7500),
                AccountCooldownKind::CapacityFreezeProbe,
                Some("剩余 2 小时 5 分"),
            ),
            (
                Some(-60),
                AccountCooldownKind::CapacityFreezeProbe,
                Some("等待探测成功"),
            ),
            (Some(20), AccountCooldownKind::RateLimit, None),
            (None, AccountCooldownKind::RateLimit, None),
        ] {
            let now = Utc::now();
            let until = seconds.map(|seconds| now + Duration::seconds(seconds));
            let facts = AccountStatusFacts {
                enabled: true,
                credential_state: CredentialState::Ready,
                access_token_expires_at: None,
                quota: QuotaState::unknown(),
                cooldown: until.map(|until| AccountCooldown {
                    until: until.into(),
                    kind,
                }),
                last_error_reason: None,
                last_error_message: None,
            };
            *fixture.account.lock().unwrap() = Some(account_page_item(facts, now, "oauth"));
            let response = admin::accounts::router::<AdminTestState>()
                .with_state(fixture.state())
                .oneshot(
                    Request::builder()
                        .uri("/api/admin/accounts")
                        .header("x-request-id", "req_cooldown_timezone")
                        .header(header::COOKIE, "cpr_session=valid-session")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), 32768).await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let quota = &value["data"]["items"][0]["quota"];
            match until {
                Some(until) => {
                    assert_eq!(
                        DateTime::parse_from_rfc3339(quota["rateLimitedUntil"].as_str().unwrap())
                            .unwrap()
                            .to_utc(),
                        until
                    );
                    let expected = expected.map(str::to_owned).unwrap_or_else(|| {
                        zone.local(until).format("%Y-%m-%d %H:%M:%S").to_string()
                    });
                    assert_eq!(quota["rateLimitRecoveryDisplay"], expected);
                }
                None => {
                    assert!(quota["rateLimitedUntil"].is_null());
                    assert!(quota["rateLimitRecoveryDisplay"].is_null());
                }
            }
        }
    }
}

fn account_page_item(
    facts: gateway_core::account::AccountStatusFacts,
    now: chrono::DateTime<chrono::Utc>,
    authentication_kind: &str,
) -> gateway_admin::model::accounts::AccountPageItem {
    use gateway_admin::model::{
        Revision,
        accounts::{AccountCapacity, AccountPageItem, AccountRecord},
    };
    use gateway_core::account::resolve_account_status;

    AccountPageItem {
        account: AccountRecord {
            outbound_proxy_enabled: true,
            id: "acct_cooldown".to_owned(),
            provider_kind: gateway_core::routing::ProviderKind::new("openai").unwrap(),
            groups: Vec::new(),
            name: "synthetic cooldown account".to_owned(),
            notes: None,
            email: None,
            upstream_user_id: None,
            upstream_account_id: None,
            plan_type: None,
            authentication_kind: authentication_kind.to_owned(),
            credential_revision: Revision::new(1).unwrap(),
            has_refresh_token: authentication_kind == "oauth",
            access_token_expires_at: None,
            next_refresh_at: None,
            enabled: true,
            concurrency_limit: None,
            weight: Default::default(),
            model_access: Default::default(),
            outbound_proxy: None,
            credential_state: facts.credential_state,
            credential_observed_at: now,
            quota: facts.quota,
            last_error_reason: None,
            last_error_message: None,
            created_at: now,
            updated_at: now,
        },
        capacity: AccountCapacity {
            used_slots: None,
            total_slots: None,
        },
        projection: resolve_account_status(&facts, now.into()),
    }
}

#[tokio::test]
async fn account_usage_returns_distinct_cache_read_and_write_tokens_without_aliases() {
    use chrono::Utc;
    use gateway_admin::model::accounts::AccountUsage;
    use gateway_core::account::{AccountStatusFacts, CredentialState, QuotaState};

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let now = Utc::now();
    let facts = AccountStatusFacts {
        enabled: true,
        credential_state: CredentialState::Ready,
        access_token_expires_at: None,
        quota: QuotaState::unknown(),
        cooldown: None,
        last_error_reason: None,
        last_error_message: None,
    };
    *fixture.account.lock().unwrap() = Some(account_page_item(facts, now, "api_key"));
    for (read, write) in [(Some(13), Some(7)), (None, None)] {
        *fixture.account_usage.lock().unwrap() = vec![AccountUsage {
            account_id: "acct_cooldown".to_owned(),
            request_count: 1,
            success_count: 1,
            input_tokens: Some(100),
            output_tokens: Some(20),
            cached_input_tokens: read,
            cache_write_input_tokens: write,
            reasoning_output_tokens: Some(3),
            image_input_tokens: None,
            image_output_tokens: None,
            image_request_count: 0,
            image_request_failed_count: 0,
            total_tokens: Some(120),
            cost_coverage: Default::default(),
            costs: Vec::new(),
            last_used_at: Some(now),
            request_buckets: Vec::new(),
            models: Vec::new(),
        }];
        let response = admin::accounts::router::<AdminTestState>()
            .with_state(fixture.state())
            .oneshot(
                Request::builder()
                    .uri("/api/admin/accounts")
                    .header("x-request-id", "req_account_usage_contract")
                    .header(header::COOKIE, "cpr_session=valid-session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 32768).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, StatusCode::OK, "{value}");
        let usage = &value["data"]["items"][0]["usage"];
        assert_eq!(usage["cachedInputTokens"], serde_json::json!(read));
        assert_eq!(usage["cacheWriteInputTokens"], serde_json::json!(write));
        assert_eq!(
            usage["cachedInputTokensDisplay"],
            if read.is_some() { "13" } else { "—" }
        );
        assert_eq!(
            usage["cacheWriteInputTokensDisplay"],
            if write.is_some() { "7" } else { "—" }
        );
        for alias in [
            "createdTokens",
            "createdTokensDisplay",
            "readTokens",
            "readTokensDisplay",
        ] {
            assert!(usage.get(alias).is_none(), "obsolete token alias: {alias}");
        }
    }
}

#[tokio::test]
async fn standalone_credential_rotation_route_is_not_exposed() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let response = admin::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/rotate")
                .header(header::COOKIE, "cpr_session=valid-session")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn connection_update_requires_admin_and_validates_before_calling_the_service() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let input = serde_json::json!({
        "accountId":"acct_api",
        "enabled":true,
        "concurrencyLimit":null,
        "weight":1,
        "groupIds":[],
        "connection":{"baseUrl":"https://api.example.invalid/v1", "transport":"http"}
    });
    for (authenticated, transport, oauth, expected) in [
        (false, "http", false, StatusCode::UNAUTHORIZED),
        (true, "invalid", false, StatusCode::BAD_REQUEST),
        // 夹具没有凭据 Store，合法输入必须进入服务，不能按普通设置静默保存
        (true, "http", false, StatusCode::SERVICE_UNAVAILABLE),
        (true, "http", true, StatusCode::SERVICE_UNAVAILABLE),
    ] {
        let mut input = input.clone();
        input["connection"]["transport"] = serde_json::json!(transport);
        if oauth {
            input["connection"]
                .as_object_mut()
                .unwrap()
                .remove("baseUrl");
        }
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/admin/accounts/update")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-request-id", "req_connection_update");
        if authenticated {
            request = request.header(header::COOKIE, "cpr_session=valid-session");
        }
        let response = admin::router::<AdminTestState>()
            .with_state(fixture.state())
            .oneshot(request.body(Body::from(input.to_string())).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
}

#[tokio::test]
async fn personal_info_requires_admin_and_a_valid_account_query() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    for (query, authenticated, expected) in [
        ("?accountId=acct_test", false, StatusCode::UNAUTHORIZED),
        ("", true, StatusCode::BAD_REQUEST),
        ("?accountId=bad", true, StatusCode::BAD_REQUEST),
        (
            "?accountId=acct_test&refresh=true",
            true,
            StatusCode::BAD_REQUEST,
        ),
        // 此夹具未提供账号 Store，合法查询应透传服务不可用，而非绕过查询
        (
            "?accountId=acct_test",
            true,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    ] {
        let mut request = Request::builder()
            .uri(format!("/api/admin/accounts/personal-info{query}"))
            .header("x-request-id", "req_personal_info");
        if authenticated {
            request = request.header(header::COOKIE, "cpr_session=valid-session");
        }
        let response = admin::router::<AdminTestState>()
            .with_state(fixture.state())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{query}");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
}

#[tokio::test]
async fn quota_forecast_requires_admin_and_valid_account_query() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    for (uri, authenticated, expected) in [
        (
            "/api/admin/accounts/quota-forecast?accountId=acct_test",
            false,
            StatusCode::UNAUTHORIZED,
        ),
        (
            "/api/admin/accounts/quota-forecast",
            true,
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/admin/accounts/quota-forecast?accountId=bad",
            true,
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/admin/accounts/quota-forecast?accountId=acct_test&refresh=true",
            true,
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let mut request = Request::builder()
            .uri(uri)
            .header("x-request-id", "req_forecast");
        if authenticated {
            request = request.header(header::COOKIE, "cpr_session=valid-session");
        }
        let response = admin::router::<AdminTestState>()
            .with_state(fixture.state())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{uri}");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = to_bytes(response.into_body(), 8192).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(value["data"].is_null());
        assert!(value["message"].is_string());
    }
}

#[tokio::test]
async fn concurrency_details_require_admin_validate_paging_and_report_lease_time() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    *fixture.account.lock().unwrap() = Some(account_page_item(
        gateway_core::account::AccountStatusFacts {
            enabled: true,
            credential_state: gateway_core::account::CredentialState::Ready,
            access_token_expires_at: None,
            quota: gateway_core::account::QuotaState::unknown(),
            cooldown: None,
            last_error_reason: None,
            last_error_message: None,
        },
        chrono::Utc::now(),
        "api_key",
    ));
    let router = admin::accounts::router::<AdminTestState>().with_state(fixture.state());
    for (authenticated, query, expected) in [
        (false, "accountId=acct_cooldown", StatusCode::UNAUTHORIZED),
        (
            true,
            "accountId=acct_cooldown&page=0",
            StatusCode::BAD_REQUEST,
        ),
        (
            true,
            "accountId=acct_cooldown&pageSize=101",
            StatusCode::BAD_REQUEST,
        ),
        (
            true,
            "accountId=acct_cooldown&page=4294967295&pageSize=100",
            StatusCode::BAD_REQUEST,
        ),
        (true, "accountId=acct_missing", StatusCode::NOT_FOUND),
        (
            true,
            "accountId=acct_cooldown&page=2&pageSize=20",
            StatusCode::OK,
        ),
    ] {
        let mut request = Request::builder()
            .uri(format!("/api/admin/accounts/concurrency?{query}"))
            .header("x-request-id", "req_concurrency");
        if authenticated {
            request = request.header(header::COOKIE, "cpr_session=valid-session");
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{query}");
        if expected == StatusCode::OK {
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            let body = to_bytes(response.into_body(), 8192).await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["data"]["total"], 21);
            assert_eq!(value["data"]["page"], 2);
            assert_eq!(value["data"]["entries"][0]["occupiedMs"], 5000);
            assert_eq!(value["data"]["entries"][0]["clientIp"], "2001:db8::376");
            assert_eq!(
                value["data"]["entries"][0]["startedAtDisplay"],
                "2026-10-17 07:59:45"
            );
        }
    }
}
