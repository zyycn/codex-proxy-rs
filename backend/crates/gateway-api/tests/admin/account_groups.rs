use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use gateway_api::admin::account_groups;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::{AdminTestFixture, AdminTestState, PRIMARY_GROUP_ID};

const SESSION_COOKIE: &str = "cpr_admin_session=valid-session";

#[tokio::test]
async fn list_route_should_keep_camel_case_group_and_page_wire() {
    let fixture = authenticated_fixture().await;
    let response = request(
        router(&fixture),
        Method::GET,
        "/api/admin/account-groups?page=1&pageSize=1&search=alpha&enabled=true",
        None,
        true,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let value = response_json(response).await;
    assert_eq!(value["data"]["page"]["page"], 1);
    assert_eq!(value["data"]["page"]["pageSize"], 1);
    assert_eq!(value["data"]["page"]["total"], 1);
    assert_eq!(value["data"]["page"]["totalPages"], 1);
    assert_eq!(value["data"]["configRevision"], 7);
    assert_eq!(value["data"]["items"][0]["id"], PRIMARY_GROUP_ID);
    assert_eq!(value["data"]["items"][0]["memberCount"], 2);
    assert_eq!(value["data"]["items"][0]["providerCounts"]["openai"], 1);
    assert_eq!(value["data"]["items"][0]["clientKeyCount"], 2);
    assert_eq!(value["data"]["items"][0]["accountSummary"]["available"], 1);
    assert_eq!(value["data"]["items"][0]["accountSummary"]["limited"], 1);
    assert_eq!(value["data"]["items"][0]["accountSummary"]["total"], 2);
    assert_eq!(value["data"]["items"][0]["capacity"]["usedSlots"], 0);
    assert_eq!(value["data"]["items"][0]["capacity"]["totalSlots"], 1);
    assert_eq!(value["data"]["items"][0]["usage"]["todayUsd"], "1.25");
    assert_eq!(value["data"]["items"][0]["usage"]["totalUsd"], "5.5");
    assert!(value["data"]["items"][0].get("member_count").is_none());
}

#[tokio::test]
async fn members_route_should_keep_provider_kind_and_member_wire() {
    let fixture = authenticated_fixture().await;
    let response = request(
        router(&fixture),
        Method::GET,
        &format!("/api/admin/account-groups/members?id={PRIMARY_GROUP_ID}"),
        None,
        true,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let value = response_json(response).await;
    assert_eq!(value["data"]["id"], PRIMARY_GROUP_ID);
    assert_eq!(value["data"]["total"], 2);
    assert_eq!(value["data"]["items"][0]["providerKind"], "openai");
    assert_eq!(value["data"]["items"][0]["email"], "openai@example.invalid");
    assert!(value["data"]["items"][0].get("provider_kind").is_none());
}

#[tokio::test]
async fn create_route_should_create_an_empty_group() {
    let fixture = authenticated_fixture().await;
    let response = request(
        router(&fixture),
        Method::POST,
        "/api/admin/account-groups/create",
        Some(json!({
            "name": "Gamma routing",
            "description": "New traffic pool",
            "color": "#a855f780"
        })),
        true,
    )
    .await;

    assert_eq!(response.status(), StatusCode::CREATED);
    let value = response_json(response).await;
    assert!(
        value["data"]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("grp_") && id.len() == 36)
    );
    assert_eq!(value["data"]["record"]["name"], "Gamma routing");
    assert_eq!(value["data"]["record"]["memberCount"], 0);
    assert_eq!(value["data"]["record"]["color"], "#A855F780");
    assert_eq!(value["data"]["configRevision"], 8);
}

#[tokio::test]
async fn update_route_should_replace_group_fields() {
    let fixture = authenticated_fixture().await;
    let response = request(
        router(&fixture),
        Method::POST,
        "/api/admin/account-groups/update",
        Some(json!({
            "id": PRIMARY_GROUP_ID,
            "name": "Renamed routing",
            "description": null,
            "color": "#F43F5ECC"
        })),
        true,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let value = response_json(response).await;
    assert_eq!(value["data"]["record"]["name"], "Renamed routing");
    assert!(value["data"]["record"]["description"].is_null());
    assert_eq!(value["data"]["record"]["color"], "#F43F5ECC");
}

#[tokio::test]
async fn enable_and_disable_routes_should_set_explicit_group_state() {
    let fixture = authenticated_fixture().await;
    let disabled = request(
        router(&fixture),
        Method::POST,
        "/api/admin/account-groups/disable",
        Some(json!({ "id": PRIMARY_GROUP_ID })),
        true,
    )
    .await;
    let enabled = request(
        router(&fixture),
        Method::POST,
        "/api/admin/account-groups/enable",
        Some(json!({ "id": PRIMARY_GROUP_ID })),
        true,
    )
    .await;

    assert_eq!(disabled.status(), StatusCode::OK);
    assert!(
        !response_json(disabled).await["data"]["record"]["enabled"]
            .as_bool()
            .expect("disabled flag")
    );
    assert_eq!(enabled.status(), StatusCode::OK);
    assert!(
        response_json(enabled).await["data"]["record"]["enabled"]
            .as_bool()
            .expect("enabled flag")
    );
}

#[tokio::test]
async fn delete_route_should_return_deleted_id_without_a_live_record() {
    let fixture = authenticated_fixture().await;
    let response = request(
        router(&fixture),
        Method::POST,
        "/api/admin/account-groups/delete",
        Some(json!({ "id": PRIMARY_GROUP_ID })),
        true,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let value = response_json(response).await;
    assert_eq!(value["data"]["id"], PRIMARY_GROUP_ID);
    assert!(value["data"]["record"].is_null());
}

#[tokio::test]
async fn list_route_should_reject_unknown_and_invalid_pagination_fields() {
    let fixture = authenticated_fixture().await;
    for uri in [
        "/api/admin/account-groups?other=true",
        "/api/admin/account-groups?page=0",
        "/api/admin/account-groups?pageSize=0",
        "/api/admin/account-groups?pageSize=201",
    ] {
        let response = request(router(&fixture), Method::GET, uri, None, true).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
    }
}

#[tokio::test]
async fn mutation_route_should_reject_unknown_json_fields() {
    let fixture = authenticated_fixture().await;
    let response = request(
        router(&fixture),
        Method::POST,
        "/api/admin/account-groups/create",
        Some(json!({
            "name": "Unknown field",
            "description": null,
            "color": "#2563EBFF",
            "providerKind": "openai"
        })),
        true,
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn create_route_should_reject_six_digit_color_without_alpha() {
    let fixture = authenticated_fixture().await;
    let response = request(
        router(&fixture),
        Method::POST,
        "/api/admin/account-groups/create",
        Some(json!({
            "name": "Opaque legacy color",
            "description": null,
            "color": "#2563EB"
        })),
        true,
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn account_group_routes_should_require_admin_authentication() {
    let fixture = AdminTestFixture::new().await;
    let get_response = request(
        router(&fixture),
        Method::GET,
        "/api/admin/account-groups",
        None,
        false,
    )
    .await;
    let post_response = request(
        router(&fixture),
        Method::POST,
        "/api/admin/account-groups/create",
        Some(json!({ "name": "Unauthorized", "description": null, "color": "#2563EBFF" })),
        false,
    )
    .await;

    assert_eq!(get_response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(post_response.status(), StatusCode::UNAUTHORIZED);
}

async fn authenticated_fixture() -> AdminTestFixture {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    fixture
}

fn router(fixture: &AdminTestFixture) -> Router {
    account_groups::router::<AdminTestState>().with_state(fixture.state())
}

async fn request(
    router: Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    authenticated: bool,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", "req_account_groups");
    if authenticated {
        builder = builder.header(header::COOKIE, SESSION_COOKIE);
    }
    let body = match body {
        Some(value) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&value).expect("serialize request body"))
        }
        None => Body::empty(),
    };
    router
        .oneshot(builder.body(body).expect("account group request"))
        .await
        .expect("account group response")
}

async fn response_json(response: axum::response::Response) -> Value {
    let body = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("account group response body");
    serde_json::from_slice(&body).expect("account group response JSON")
}
