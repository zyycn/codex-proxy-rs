use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use gateway_api::admin;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::{AdminTestFixture, AdminTestState};

const SESSION_COOKIE: &str = "cpr_admin_session=valid-session";

fn app(state: AdminTestState) -> Router {
    admin::router::<AdminTestState>().with_state(state)
}

fn request(method: Method, uri: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", "req_admin_errors")
        .body(body)
        .expect("build admin error request")
}

async fn response_json(response: axum::response::Response) -> (StatusCode, String, Value) {
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read admin error response body");
    let body = serde_json::from_slice(&body).expect("parse admin error response JSON");
    (status, content_type, body)
}

#[tokio::test]
async fn malformed_login_json_should_use_the_admin_error_envelope() {
    let fixture = AdminTestFixture::new().await;
    let mut request = request(
        Method::POST,
        "/api/admin/auth/login",
        Body::from(r#"{"password":"secret""#),
    );
    request
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());

    let response = app(fixture.state())
        .oneshot(request)
        .await
        .expect("malformed JSON response");
    let actual = response_json(response).await;

    assert_eq!(
        actual,
        (
            StatusCode::BAD_REQUEST,
            "application/json".to_owned(),
            json!({
                "code": 40000,
                "message": "请求体不是合法 JSON",
                "data": null
            })
        )
    );
}

#[tokio::test]
async fn invalid_login_json_data_should_not_echo_the_submitted_value() {
    let fixture = AdminTestFixture::new().await;
    let submitted = "password-must-not-leak";
    let mut request = request(
        Method::POST,
        "/api/admin/auth/login",
        Body::from(json!({ "password": submitted, "rememberMe": true }).to_string()),
    );
    request
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());

    let response = app(fixture.state())
        .oneshot(request)
        .await
        .expect("invalid JSON data response");
    let actual = response_json(response).await;

    assert_eq!(
        actual,
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "application/json".to_owned(),
            json!({
                "code": 40001,
                "message": "请求字段不合法",
                "data": null
            })
        )
    );
    assert!(!actual.2.to_string().contains(submitted));
}

#[tokio::test]
async fn login_without_json_content_type_should_keep_415_with_an_admin_envelope() {
    let fixture = AdminTestFixture::new().await;
    let response = app(fixture.state())
        .oneshot(request(
            Method::POST,
            "/api/admin/auth/login",
            Body::from(json!({ "password": "secret" }).to_string()),
        ))
        .await
        .expect("missing JSON content type response");
    let actual = response_json(response).await;

    assert_eq!(
        actual,
        (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "application/json".to_owned(),
            json!({
                "code": 40001,
                "message": "请求必须使用 application/json",
                "data": null
            })
        )
    );
}

#[tokio::test]
async fn malformed_admin_query_should_use_a_safe_bad_request_envelope() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let mut request = request(
        Method::GET,
        "/api/admin/usage/records?page=not-a-number",
        Body::empty(),
    );
    request
        .headers_mut()
        .insert(header::COOKIE, SESSION_COOKIE.parse().unwrap());

    let response = app(fixture.state())
        .oneshot(request)
        .await
        .expect("invalid query response");
    let actual = response_json(response).await;

    assert_eq!(
        actual,
        (
            StatusCode::BAD_REQUEST,
            "application/json".to_owned(),
            json!({
                "code": 40001,
                "message": "请求参数不合法",
                "data": null
            })
        )
    );
}

#[tokio::test]
async fn invalid_time_range_should_use_its_published_business_code() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let mut request = request(
        Method::GET,
        "/api/admin/dashboard/summary?startTime=not-a-time",
        Body::empty(),
    );
    request
        .headers_mut()
        .insert(header::COOKIE, SESSION_COOKIE.parse().unwrap());

    let response = app(fixture.state())
        .oneshot(request)
        .await
        .expect("invalid time range response");
    let actual = response_json(response).await;

    assert_eq!(
        actual,
        (
            StatusCode::BAD_REQUEST,
            "application/json".to_owned(),
            json!({
                "code": 40002,
                "message": "时间范围不合法",
                "data": null
            })
        )
    );
}

#[tokio::test]
async fn unknown_admin_path_should_not_fall_through_to_the_spa() {
    let fixture = AdminTestFixture::new().await;
    let response = app(fixture.state())
        .oneshot(request(
            Method::GET,
            "/api/admin/not-a-real-route",
            Body::empty(),
        ))
        .await
        .expect("unknown admin path response");
    let actual = response_json(response).await;

    assert_eq!(
        actual,
        (
            StatusCode::NOT_FOUND,
            "application/json".to_owned(),
            json!({
                "code": 40401,
                "message": "管理接口不存在",
                "data": null
            })
        )
    );
}

#[tokio::test]
async fn unsupported_admin_method_should_keep_allow_and_return_an_envelope() {
    let fixture = AdminTestFixture::new().await;
    let response = app(fixture.state())
        .oneshot(request(
            Method::POST,
            "/api/admin/auth/status",
            Body::empty(),
        ))
        .await
        .expect("unsupported admin method response");
    let allow = response
        .headers()
        .get(header::ALLOW)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let actual = response_json(response).await;

    assert_eq!(
        (allow, actual),
        (
            "GET,HEAD".to_owned(),
            (
                StatusCode::METHOD_NOT_ALLOWED,
                "application/json".to_owned(),
                json!({
                    "code": 40001,
                    "message": "请求方法不受支持",
                    "data": null
                })
            )
        )
    );
}

#[tokio::test]
async fn admin_auth_failures_should_use_stable_chinese_contracts() {
    let fixture = AdminTestFixture::new().await;
    let unauthenticated = app(fixture.state())
        .clone()
        .oneshot(request(Method::GET, "/api/admin/accounts", Body::empty()))
        .await
        .expect("missing session response");
    let mut invalid_api_key = request(Method::GET, "/api/admin/accounts", Body::empty());
    invalid_api_key
        .headers_mut()
        .insert("x-api-key", "admin-invalid".parse().unwrap());
    let invalid_api_key = app(fixture.state())
        .clone()
        .oneshot(invalid_api_key)
        .await
        .expect("invalid API key response");
    let mut invalid_login = request(
        Method::POST,
        "/api/admin/auth/login",
        Body::from(json!({ "password": "wrong-password" }).to_string()),
    );
    invalid_login
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    let invalid_login = app(fixture.state())
        .oneshot(invalid_login)
        .await
        .expect("invalid credentials response");

    let actual = [
        response_json(unauthenticated).await,
        response_json(invalid_login).await,
        response_json(invalid_api_key).await,
    ];

    assert_eq!(
        actual.map(|(status, _, body)| (status, body["code"].clone(), body["message"].clone())),
        [
            (
                StatusCode::UNAUTHORIZED,
                json!(40101),
                json!("需要管理员登录")
            ),
            (
                StatusCode::UNAUTHORIZED,
                json!(40102),
                json!("管理员用户名或密码错误")
            ),
            (
                StatusCode::UNAUTHORIZED,
                json!(40103),
                json!("管理 API Key 无效")
            ),
        ]
    );
}
