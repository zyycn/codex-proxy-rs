use chrono::{DateTime, TimeZone as _, Utc};
use gateway_api::admin::client_keys::{
    self, ClientKeyCursorData, ClientKeyCursorValue, ClientKeyListData, ClientKeyMutationRequest,
    ClientKeySort, ClientKeySortDirection, ClientKeySortField, ClientKeyView,
    CreateClientKeyRequest, CreatedClientKeyData, ListClientKeysQuery, MutatedClientKeyData,
    RevealedClientKeyData, UpdateClientKeyRequest, decode_client_key_cursor,
    encode_client_key_cursor,
};
use serde_json::json;

use super::{AdminTestFixture, AdminTestState};

#[test]
fn reset_budget_requires_an_explicit_supported_period_and_valid_key() {
    use gateway_api::admin::client_keys::ResetClientKeyBudgetRequest;
    for period in ["daily", "weekly", "all"] {
        let command = serde_json::from_value::<ResetClientKeyBudgetRequest>(
            json!({"id":"key_reset", "period":period}),
        )
        .unwrap()
        .into_command()
        .unwrap();
        assert_eq!(command.id.as_str(), "key_reset");
    }
    for payload in [
        json!({"id":"key_reset"}),
        json!({"id":"key_reset", "period":"monthly"}),
        json!({"id":"key_reset", "period":"all", "amount":0}),
    ] {
        assert!(serde_json::from_value::<ResetClientKeyBudgetRequest>(payload).is_err());
    }
    assert!(
        serde_json::from_value::<ResetClientKeyBudgetRequest>(json!({"id":" ", "period":"all"}))
            .unwrap()
            .into_command()
            .is_err()
    );
}

#[tokio::test]
async fn reset_budget_route_requires_admin_and_maps_missing_keys() {
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use tower::ServiceExt as _;
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    for (cookie, expected) in [
        ("", StatusCode::UNAUTHORIZED),
        ("cpr_session=valid-session", StatusCode::NOT_FOUND),
    ] {
        let response = client_keys::router::<AdminTestState>()
            .with_state(fixture.state())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/client-keys/reset-budget")
                    .header(header::COOKIE, cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-request-id", "req_reset")
                    .body(Body::from(r#"{"id":"missing","period":"all"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
}

#[tokio::test]
async fn budget_reset_allows_admin_but_rejects_key_sessions_without_changing_usage() {
    use crate::support::{RAW_KEY, json_request, key_fixture, response_json};
    use axum::http::{Method, StatusCode, header};
    use gateway_core::policy::ClientApiKeyId;
    use tower::ServiceExt as _;

    let fixture = key_fixture().await;
    fixture.auth.insert_session("valid-admin");
    let mut record = fixture
        .services
        .client_keys()
        .reveal(&ClientApiKeyId::new("key-42").unwrap())
        .await
        .unwrap()
        .record;
    record.budget.daily_used_usd = "1.25".parse().unwrap();
    record.budget.weekly_used_usd = "4.5".parse().unwrap();
    *fixture.client_key.lock().unwrap() = Some(record);
    let app = crate::openai::api_router_with_admin(fixture.services.clone());
    let login = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/auth/login",
            json!({"mode":"key", "apiKey":RAW_KEY}),
        ))
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let key_cookie = login.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    for (cookie, expected, used) in [
        (key_cookie.as_str(), StatusCode::FORBIDDEN, "1.25"),
        ("cpr_session=valid-admin", StatusCode::OK, "0"),
    ] {
        let mut request = json_request(
            Method::POST,
            "/api/admin/client-keys/reset-budget",
            json!({"id":"key-42", "period":"daily"}),
        );
        request
            .headers_mut()
            .insert(header::COOKIE, cookie.parse().unwrap());
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected);
        if expected == StatusCode::OK {
            assert_eq!(
                response_json(response).await["data"],
                json!({"id":"key-42"})
            );
        }
        let key = fixture.client_key.lock().unwrap().clone().unwrap();
        assert_eq!(key.budget.daily_used_usd.canonical(), used);
        assert_eq!(key.budget.weekly_used_usd.canonical(), "4.5");
    }
}

#[test]
fn custom_client_keys_preserve_migrated_values_and_redact_debug() {
    for value in [
        "q".to_owned(),
        "sk-old!@/key+=value".to_owned(),
        "x".repeat(8192),
    ] {
        let request: CreateClientKeyRequest = serde_json::from_value(json!({
            "name": "migration", "customKey": value, "groupIds": [],
            "maxConcurrency": 2, "requestsPerMinute": 0
        }))
        .unwrap();
        let debug = format!("{request:?}");
        assert!(debug.contains("[REDACTED]"));
        let command = request.into_command().unwrap();
        assert_eq!(command.custom_key.unwrap().expose_for_auth(), value);
    }
    let secret = "legacy-key-should-not-leak";
    let request: CreateClientKeyRequest = serde_json::from_value(json!({
        "name": "migration", "customKey": secret, "groupIds": [],
        "maxConcurrency": 0, "requestsPerMinute": 0
    }))
    .unwrap();
    assert!(!format!("{request:?}").contains(secret));
    assert!(!format!("{:?}", request.into_command().unwrap()).contains(secret));
}

#[test]
fn custom_client_key_is_optional_and_only_rejects_untransportable_values() {
    let payload = json!({"name": "migration", "groupIds": [], "maxConcurrency": 0,
        "requestsPerMinute": 0});
    for value in [None, Some(json!(null)), Some(json!(""))] {
        let mut payload = payload.clone();
        if let Some(value) = value {
            payload["customKey"] = value;
        }
        let command = serde_json::from_value::<CreateClientKeyRequest>(payload)
            .unwrap()
            .into_command()
            .unwrap();
        assert!(command.custom_key.is_none());
    }
    for value in [
        " key",
        "key ",
        "key with spaces",
        "key\n",
        "key\tvalue",
        "密钥",
    ] {
        let mut payload = payload.clone();
        payload["customKey"] = json!(value);
        let error = serde_json::from_value::<CreateClientKeyRequest>(payload)
            .unwrap()
            .into_command()
            .unwrap_err();
        assert_eq!(error.field(), "customKey");
        assert!(!format!("{error:?}").contains(value));
    }
    let mut update = payload;
    update["id"] = json!("key_existing");
    update["customKey"] = json!("replacement-credential");
    assert!(serde_json::from_value::<UpdateClientKeyRequest>(update).is_err());
}

#[test]
fn budget_inputs_preserve_decimal_precision_and_omitted_updates() {
    let payload = json!({"name": "budget", "groupIds": [], "maxConcurrency": 3,
        "requestsPerMinute": 0, "dailyLimitUsd": "0.1234567891", "weeklyLimitUsd": "15"});
    let command = serde_json::from_value::<CreateClientKeyRequest>(payload.clone())
        .unwrap()
        .into_command()
        .unwrap();
    assert_eq!(command.budget.daily_usd.canonical(), "0.1234567891");
    assert_eq!(command.budget.weekly_usd.canonical(), "15");
    assert_eq!(command.limits.max_concurrency, 3);
    for field in ["dailyLimitUsd", "weeklyLimitUsd"] {
        for invalid in ["-1", "NaN", "1e3", "10000000000", "0.00000000001", ""] {
            let mut payload = payload.clone();
            payload[field] = json!(invalid);
            assert_eq!(
                serde_json::from_value::<CreateClientKeyRequest>(payload)
                    .unwrap()
                    .into_command()
                    .unwrap_err()
                    .field(),
                field
            );
        }
    }
    let mut update = payload;
    update["id"] = json!("key_budget");
    update.as_object_mut().unwrap().remove("dailyLimitUsd");
    update["weeklyLimitUsd"] = json!("0");
    let command = serde_json::from_value::<UpdateClientKeyRequest>(update)
        .unwrap()
        .into_command()
        .unwrap();
    assert_eq!(command.daily_limit_usd, None);
    assert_eq!(
        command.weekly_limit_usd,
        Some(gateway_core::metering::Decimal::ZERO)
    );
}

#[test]
fn client_key_queries_should_reject_unknown_zero_and_oversized_fields() {
    let unknown = serde_json::from_value::<ListClientKeysQuery>(json!({ "other": true }));
    let zero = serde_json::from_value::<ListClientKeysQuery>(json!({ "limit": 0 }))
        .expect("deserialize zero limit");
    let oversized = serde_json::from_value::<ListClientKeysQuery>(json!({
        "cursor": "a".repeat(513)
    }))
    .expect("deserialize oversized cursor");
    let oversized_search = serde_json::from_value::<ListClientKeysQuery>(json!({
        "search": "a".repeat(257)
    }))
    .expect("deserialize oversized search");
    let invalid_sort = serde_json::from_value::<ListClientKeysQuery>(json!({
        "sortBy": "plaintextKey",
        "sortDirection": "asc"
    }))
    .expect("deserialize invalid sort");

    assert!(unknown.is_err());
    assert_eq!(
        zero.into_command().expect_err("reject zero limit").field(),
        "limit"
    );
    assert_eq!(
        oversized
            .into_command()
            .expect_err("reject oversized cursor")
            .field(),
        "cursor"
    );
    assert_eq!(
        oversized_search
            .into_command()
            .expect_err("reject oversized search")
            .field(),
        "search"
    );
    assert_eq!(
        invalid_sort
            .into_command()
            .expect_err("reject invalid sort")
            .field(),
        "sortBy"
    );
    let sorted = serde_json::from_value::<ListClientKeysQuery>(json!({
        "sortBy": "lastUsedAt",
        "sortDirection": "asc"
    }))
    .expect("deserialize valid sort")
    .into_command()
    .expect("validate sort");
    assert_eq!(
        sorted.sort.field,
        gateway_admin::model::client_keys::ClientKeySortField::LastUsedAt,
    );
    assert_eq!(
        sorted.sort.direction,
        gateway_admin::model::client_keys::SortDirection::Asc,
    );
    assert_eq!(
        serde_json::from_value::<ListClientKeysQuery>(json!({ "search": "  " }))
            .expect("deserialize blank search")
            .into_command()
            .expect("normalize blank search")
            .search,
        None
    );
    assert_eq!(
        serde_json::from_value::<ListClientKeysQuery>(json!({ "limit": u16::MAX }))
            .expect("deserialize maximum u16 page size")
            .into_command()
            .expect("accept maximum u16 page size")
            .page_size
            .get(),
        u16::MAX
    );
}

#[test]
fn client_key_name_search_does_not_interpret_names_as_credentials() {
    let name = format!("sk_{}", "a".repeat(43));
    let command = serde_json::from_value::<ListClientKeysQuery>(json!({ "search": name }))
        .unwrap()
        .into_command()
        .unwrap();
    assert_eq!(command.search.as_deref(), Some(name.as_str()));
}

#[test]
fn client_key_mutations_should_validate_text_limits_and_unknown_fields() {
    let valid = serde_json::from_value::<CreateClientKeyRequest>(json!({
        "name": "terminal key",
        "label": "production",
        "groupIds": [],
        "maxConcurrency": 2,
        "requestsPerMinute": 60
    }))
    .expect("deserialize create")
    .into_command()
    .expect("validate create");
    assert_eq!(valid.name, "terminal key");

    for (payload, field) in [
        (
            json!({
                "name": " ",
                "groupIds": [],
                "maxConcurrency": 0,
                "requestsPerMinute": 0
            }),
            "name",
        ),
        (
            json!({
                "name": "key",
                "groupIds": [],
                "maxConcurrency": u64::MAX,
                "requestsPerMinute": 0
            }),
            "maxConcurrency",
        ),
    ] {
        let request = serde_json::from_value::<CreateClientKeyRequest>(payload)
            .expect("deserialize invalid create shape");
        assert_eq!(
            request
                .into_command()
                .expect_err("reject invalid create")
                .field(),
            field
        );
    }

    assert!(
        serde_json::from_value::<UpdateClientKeyRequest>(json!({
            "id": "key_1",
            "expectedConfigRevision": 1,
            "name": "key",
            "groupIds": [],
            "maxConcurrency": 0,
            "requestsPerMinute": 0,
            "tokensPerMinute": 0
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<CreateClientKeyRequest>(json!({
            "expectedConfigRevision": 7,
            "name": "terminal key",
            "groupIds": [],
            "maxConcurrency": 2,
            "requestsPerMinute": 60
        }))
        .is_err()
    );
    let mutation_id = serde_json::from_value::<ClientKeyMutationRequest>(json!({
        "id": "key_1"
    }))
    .expect("deserialize mutation")
    .into_id()
    .expect("validate mutation");
    assert_eq!(mutation_id, "key_1");
}

#[test]
fn client_key_cursor_should_round_trip_and_reject_noncanonical_input() {
    let created_at = Utc
        .with_ymd_and_hms(2026, 7, 18, 8, 0, 0)
        .single()
        .expect("valid time");
    let cursor = ClientKeyCursorData {
        sort: ClientKeySort {
            field: ClientKeySortField::CreatedAt,
            direction: ClientKeySortDirection::Desc,
        },
        value: ClientKeyCursorValue::CreatedAt(created_at),
        id: "key_cursor".to_owned(),
    };
    let encoded = encode_client_key_cursor(&cursor).expect("encode cursor");
    let decoded = decode_client_key_cursor(&encoded).expect("decode cursor");

    assert_eq!(decoded, cursor);
    assert!(!encoded.contains("key_cursor"));
    for invalid in ["", "not-base64!", "e30"] {
        assert!(decode_client_key_cursor(invalid).is_err());
    }
}

#[test]
fn client_key_responses_should_keep_shape_and_redact_creation_debug() {
    let created_at = Utc
        .with_ymd_and_hms(2026, 7, 18, 8, 0, 0)
        .single()
        .expect("valid time");
    let view = ClientKeyView::from(gateway_admin::model::client_keys::ClientKeyRecord {
        openai_client_profile_override: None,
        xai_client_profile_override: None,
        budget: Default::default(),
        id: gateway_core::policy::ClientApiKeyId::new("key_visible").expect("Client Key ID"),
        name: "visible".to_owned(),
        label: None,
        groups: Vec::new(),
        provider_kinds: vec![
            gateway_core::routing::ProviderKind::new("openai").expect("Provider kind"),
        ],
        prefix: "sk_visible12".to_owned(),
        enabled: true,
        limits: gateway_core::policy::RateLimits {
            max_concurrency: 2,
            requests_per_minute: 60,
        },
        created_at,
        updated_at: created_at,
        last_used_at: Some(created_at),
    });
    let list =
        serde_json::to_value(ClientKeyListData::new(vec![view], None, 1)).expect("serialize list");
    assert!(list.get("configRevision").is_none());
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], "key_visible");
    assert_eq!(
        list["items"][0]["providerKinds"],
        serde_json::json!(["openai"])
    );
    assert_eq!(list["items"][0]["routingScope"], "all");
    assert_eq!(list["items"][0]["maxConcurrency"], 2);
    assert_eq!(list["items"][0]["requestsPerMinute"], 60);
    assert!(list["items"][0].get("tokensPerMinute").is_none());
    assert!(list["items"][0].get("policyJson").is_none());
    assert_eq!(list["items"][0]["dailyLimitUsd"], "0");
    assert_eq!(list["items"][0]["weeklyLimitUsd"], "0");
    assert_eq!(list["items"][0]["dailyUsedUsd"], "0");
    assert!(list["items"][0].get("unresolvedRequests").is_none());
    assert_eq!(
        DateTime::parse_from_rfc3339(
            list["items"][0]["lastUsedAt"]
                .as_str()
                .expect("serialized last-used timestamp")
        )
        .expect("parse serialized last-used timestamp"),
        created_at.fixed_offset()
    );
    assert!(list["items"][0].get("plaintextKey").is_none());

    let plaintext = format!("sk_{}", "a".repeat(43));
    let created = CreatedClientKeyData::new(
        "key_created".to_owned(),
        "sk_aaaaaaaaa".to_owned(),
        plaintext.clone(),
    );
    assert!(!format!("{created:?}").contains(&plaintext));
    let value = serde_json::to_value(created).expect("serialize created key");
    assert_eq!(value["plaintextKey"], plaintext);
    let revealed = RevealedClientKeyData::new("key_created".to_owned(), plaintext.clone());
    assert!(!format!("{revealed:?}").contains(&plaintext));
    assert_eq!(
        serde_json::to_value(revealed).expect("serialize revealed key"),
        json!({ "id": "key_created", "plaintextKey": plaintext })
    );
    assert_eq!(
        serde_json::to_value(MutatedClientKeyData::new("key_created".to_owned()))
            .expect("serialize mutation"),
        json!({ "id": "key_created" })
    );
}

#[tokio::test]
async fn reveal_route_should_use_query_id_and_no_store_response() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use tower::ServiceExt as _;

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let response = client_keys::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/client-keys/reveal?id=key_1")
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_reveal")
                .body(Body::empty())
                .expect("reveal request"),
        )
        .await
        .expect("reveal response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    let body = to_bytes(response.into_body(), 4096)
        .await
        .expect("reveal body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("reveal JSON");
    assert_eq!(value["data"]["id"], "key_1");
    assert_eq!(
        value["data"]["plaintextKey"],
        format!("sk_{}", "a".repeat(43))
    );
}

#[tokio::test]
async fn list_route_should_accept_the_full_nonzero_u16_page_size() {
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use tower::ServiceExt as _;

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let response = client_keys::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/client-keys?limit=65535")
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_client_keys_max_page")
                .body(Body::empty())
                .expect("client key list request"),
        )
        .await
        .expect("client key list response");

    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn profile_override_distinguishes_omission_from_explicit_inheritance() {
    let mut payload = json!({"id":"key_profile", "name":"profile", "groupIds":[], "maxConcurrency":0, "requestsPerMinute":0});
    let decode = |body| {
        serde_json::from_value::<UpdateClientKeyRequest>(body)
            .unwrap()
            .into_command()
            .unwrap()
            .openai_client_profile_override
    };
    assert_eq!(decode(payload.clone()), None);
    payload["openaiClientProfileOverride"] = json!(null);
    assert_eq!(decode(payload.clone()), Some(None));
    payload["openaiClientProfileOverride"] =
        json!({"client":"cli", "platform":"linux", "versionMode":"latest"});
    assert!(decode(payload).unwrap().is_some());
}

#[test]
fn xai_profile_override_distinguishes_omission_from_explicit_inheritance() {
    let mut payload = json!({"id":"key_profile", "name":"profile", "groupIds":[], "maxConcurrency":0, "requestsPerMinute":0});
    let decode = |body| {
        serde_json::from_value::<UpdateClientKeyRequest>(body)
            .unwrap()
            .into_command()
            .unwrap()
            .xai_client_profile_override
    };
    assert_eq!(decode(payload.clone()), None);
    payload["xaiClientProfileOverride"] = json!(null);
    assert_eq!(decode(payload.clone()), Some(None));
    payload["xaiClientProfileOverride"] = json!({"versionMode":"latest"});
    assert!(decode(payload).unwrap().is_some());
}
