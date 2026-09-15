use std::sync::atomic::Ordering;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::client::{
    RAW_KEY, client_app, cookie_request, empty_request, json_request, response_json,
};

fn session_cookie(response: &axum::response::Response) -> String {
    response.headers()[header::SET_COOKIE]
        .to_str()
        .expect("cookie")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_owned()
}

async fn login(
    app: &axum::Router,
    body: Value,
    previous: Option<&str>,
) -> axum::response::Response {
    let mut request = json_request(Method::POST, "/api/auth/login", body);
    if let Some(cookie) = previous {
        request
            .headers_mut()
            .insert(header::COOKIE, cookie.parse().expect("cookie"));
    }
    app.clone().oneshot(request).await.expect("login response")
}

async fn get(app: &axum::Router, path: &str, cookie: &str) -> axum::response::Response {
    app.clone()
        .oneshot(cookie_request(Method::GET, path, cookie))
        .await
        .expect("GET response")
}

#[tokio::test]
async fn unified_login_returns_server_identity_and_rotates_the_previous_session() {
    let (app, _) = client_app().await;
    let key = login(&app, json!({"mode": "key", "apiKey": RAW_KEY}), None).await;
    assert_eq!(key.status(), StatusCode::OK);
    let key_cookie = session_cookie(&key);
    let key_data = response_json(key).await["data"].clone();
    assert_eq!(key_data["role"], "key");
    assert!(key_data["expiresAt"].is_string());
    assert_eq!(key_data.as_object().expect("session object").len(), 2);
    assert_eq!(
        response_json(get(&app, "/api/auth/status", &key_cookie).await).await["data"],
        json!({"authenticated": true, "session": key_data})
    );

    let admin = login(
        &app,
        json!({"mode": "admin", "username": "admin_1", "password": "strong-admin-password"}),
        Some(&key_cookie),
    )
    .await;
    assert_eq!(admin.status(), StatusCode::OK);
    let admin_cookie = session_cookie(&admin);
    let admin_data = response_json(admin).await["data"].clone();
    assert_eq!(admin_data["role"], "admin");
    assert!(admin_data["expiresAt"].is_string());
    assert_eq!(admin_data.as_object().expect("session object").len(), 2);
    assert_ne!(key_cookie, admin_cookie);
    assert_eq!(
        response_json(get(&app, "/api/auth/status", &admin_cookie).await).await["data"],
        json!({"authenticated": true, "session": admin_data})
    );
    assert_eq!(
        response_json(get(&app, "/api/auth/status", &key_cookie).await).await["data"],
        json!({"authenticated": false, "session": null})
    );
    assert_eq!(
        get(&app, "/api/client/overview", &key_cookie)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&app, "/api/admin/system/version", &admin_cookie)
            .await
            .status(),
        StatusCode::OK
    );

    let key = login(
        &app,
        json!({"mode": "key", "apiKey": RAW_KEY}),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(key.status(), StatusCode::OK);
    let renewed = session_cookie(&key);
    assert_ne!(renewed, key_cookie);
    assert_eq!(
        get(&app, "/api/admin/system/version", &admin_cookie)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&app, "/api/client/overview", &renewed).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn login_rejects_legacy_type_without_replacing_the_current_session() {
    let (app, _) = client_app().await;
    let cookie =
        session_cookie(&login(&app, json!({"mode": "key", "apiKey": RAW_KEY}), None).await);
    for body in [
        json!({"type": "key", "apiKey": RAW_KEY}),
        json!({"type": "admin", "password": "strong-admin-password"}),
        json!({"mode": "key", "type": "key", "apiKey": RAW_KEY}),
        json!({"mode": "admin", "type": "admin", "password": "strong-admin-password"}),
    ] {
        let response = login(&app, body, Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert!(!response.headers().contains_key(header::SET_COOKIE));
        let error = response_json(response).await.to_string();
        assert!(!error.contains(RAW_KEY));
        assert!(!error.contains("strong-admin-password"));
    }
    assert_eq!(
        response_json(get(&app, "/api/auth/status", &cookie).await).await["data"]["session"]["role"],
        "key"
    );
}

#[tokio::test]
async fn valid_key_session_cannot_read_or_mutate_admin_resources() {
    let (app, _) = client_app().await;
    let cookie =
        session_cookie(&login(&app, json!({"mode": "key", "apiKey": RAW_KEY}), None).await);
    for path in [
        "/api/admin/system/version",
        "/api/admin/accounts",
        "/api/admin/client-keys",
        "/api/admin/settings",
    ] {
        let response = get(&app, path, &cookie).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        assert_eq!(response_json(response).await["code"], 40301);
    }
    let mut request = json_request(
        Method::POST,
        "/api/admin/settings/admin-api-key/regenerate",
        json!({}),
    );
    request
        .headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        get(&app, "/api/client/overview", &cookie).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn admin_session_is_not_a_key_session() {
    let (app, _) = client_app().await;
    let cookie = session_cookie(
        &login(
            &app,
            json!({"mode": "admin", "password": "strong-admin-password"}),
            None,
        )
        .await,
    );
    for path in [
        "/api/client/overview",
        "/api/client/system/version",
        "/api/client/usage/records?startTime=2026-09-07T00:00:00Z&endTime=2026-09-14T00:00:00Z&currentPage=1&pageSize=10",
    ] {
        assert_eq!(
            get(&app, path, &cookie).await.status(),
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    assert_eq!(
        response_json(get(&app, "/api/auth/status", &cookie).await).await["data"]["session"]["role"],
        "admin"
    );
}

#[tokio::test]
async fn declared_roles_and_foreign_key_ids_cannot_override_server_authority() {
    let (app, _) = client_app().await;
    let cookie =
        session_cookie(&login(&app, json!({"mode": "key", "apiKey": RAW_KEY}), None).await);
    for body in [
        json!({"mode": "admin", "apiKey": RAW_KEY}),
        json!({"mode": "key", "apiKey": RAW_KEY, "role": "admin"}),
        json!({"mode": "admin", "password": RAW_KEY, "role": "key"}),
        json!({"role": "key", "apiKey": RAW_KEY}),
        json!({"mode": "key", "apiKey": RAW_KEY, "adminUserId": "admin_1"}),
        json!({"mode": "key", "apiKey": RAW_KEY, "clientKeyId": "other-key"}),
        json!({"mode": "superadmin", "apiKey": RAW_KEY}),
        json!({"apiKey": RAW_KEY}),
    ] {
        let response = login(&app, body, Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!response_json(response).await.to_string().contains(RAW_KEY));
    }
    let invalid = login(
        &app,
        json!({"mode": "admin", "password": RAW_KEY}),
        Some(&cookie),
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);
    assert!(!invalid.headers().contains_key(header::SET_COOKIE));
    assert_eq!(response_json(invalid).await["code"], 40102);
    assert_eq!(
        response_json(get(&app, "/api/auth/status", &cookie).await).await["data"]["session"]["role"],
        "key"
    );
    for suffix in [
        "clientKeyId=other-key",
        "clientApiKeyRef=other-key",
        "role=admin",
    ] {
        assert_eq!(
            get(
                &app,
                &format!(
                    "/api/client/usage/records?startTime=2026-09-13T00:00:00Z&endTime=2026-09-14T00:00:00Z&currentPage=1&pageSize=10&{suffix}"
                ),
                &cookie,
            )
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn data_plane_keys_and_browser_sessions_are_not_interchangeable() {
    let (app, _) = client_app().await;
    let cookie =
        session_cookie(&login(&app, json!({"mode": "key", "apiKey": RAW_KEY}), None).await);
    assert_eq!(
        get(&app, "/v1/models", &cookie).await.status(),
        StatusCode::UNAUTHORIZED
    );
    for name in [header::AUTHORIZATION.as_str(), "x-api-key"] {
        let request = Request::get("/api/client/overview")
            .header(name, format!("Bearer {RAW_KEY}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        get(
            &app,
            "/api/client/overview",
            &format!("cpr_session={RAW_KEY}"),
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn disabled_key_revokes_its_session_and_reenabling_does_not_restore_it() {
    let (app, store) = client_app().await;
    let cookie =
        session_cookie(&login(&app, json!({"mode": "key", "apiKey": RAW_KEY}), None).await);
    store.enabled.store(false, Ordering::SeqCst);
    assert_eq!(
        response_json(get(&app, "/api/auth/status", &cookie).await).await["data"],
        json!({"authenticated": false, "session": null})
    );
    assert_eq!(
        login(&app, json!({"mode": "key", "apiKey": RAW_KEY}), None)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    store.enabled.store(true, Ordering::SeqCst);
    assert_eq!(
        get(&app, "/api/client/overview", &cookie).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn key_store_outage_is_unavailable_not_expired_or_authenticated() {
    let (app, store) = client_app().await;
    let cookie =
        session_cookie(&login(&app, json!({"mode": "key", "apiKey": RAW_KEY}), None).await);
    store.unavailable.store(true, Ordering::SeqCst);
    for path in [
        "/api/auth/status",
        "/api/client/overview",
        "/api/client/system/version",
    ] {
        let response = get(&app, path, &cookie).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert!(!response.headers().contains_key(header::SET_COOKIE));
    }
    store.unavailable.store(false, Ordering::SeqCst);
    assert_eq!(
        get(&app, "/api/client/overview", &cookie).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn removed_auth_routes_do_not_fall_back_to_spa_or_accept_old_cookies() {
    let (app, _) = client_app().await;
    for path in [
        "/api/admin/auth/login",
        "/api/client/auth/status",
        "/api/auth/unknown",
    ] {
        let response = app
            .clone()
            .oneshot(empty_request(Method::GET, path))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response_json(response).await["code"], 40401);
    }
    let cookie =
        session_cookie(&login(&app, json!({"mode": "key", "apiKey": RAW_KEY}), None).await);
    for old_name in ["cpr_admin_session", "cpr_client_session"] {
        let old_cookie = cookie.replace("cpr_session", old_name);
        assert_eq!(
            get(&app, "/api/client/overview", &old_cookie)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
}

#[test]
fn login_wire_never_prints_either_credential_in_debug() {
    for body in [
        json!({"mode": "key", "apiKey": RAW_KEY}),
        json!({"mode": "admin", "password": RAW_KEY}),
    ] {
        let request: gateway_api::auth::LoginRequest = serde_json::from_value(body).unwrap();
        let debug = format!("{request:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(RAW_KEY));
    }
}
