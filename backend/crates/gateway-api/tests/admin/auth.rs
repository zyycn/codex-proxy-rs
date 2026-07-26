use gateway_admin::model::auth::{LoginCommand, LoginError};
use gateway_api::admin::auth::{
    AdminLoginData, AdminLoginRequest, AdminLogoutData, AdminSessionStatusData,
};
use serde_json::json;

use super::AdminTestFixture;

#[test]
fn login_request_should_deny_unknown_fields_and_redact_password_debug() {
    let password = "admin-password-must-not-leak";
    let request = serde_json::from_value::<AdminLoginRequest>(json!({
        "username": "admin@example.invalid",
        "password": password
    }))
    .expect("deserialize login request");

    assert!(!format!("{request:?}").contains(password));
    let (username, parsed_password) = request.into_parts();
    assert_eq!(username.as_deref(), Some("admin@example.invalid"));
    assert_eq!(parsed_password, password);
    assert!(
        serde_json::from_value::<AdminLoginRequest>(json!({
            "password": password,
            "rememberMe": true
        }))
        .is_err()
    );
}

#[test]
fn auth_responses_should_keep_stable_wire_shapes() {
    assert_eq!(
        serde_json::to_value(AdminLoginData::new("2026-07-18T08:00:00+08:00".to_owned()))
            .expect("serialize login"),
        json!({ "expiresAt": "2026-07-18T08:00:00+08:00" })
    );
    assert_eq!(
        serde_json::to_value(AdminSessionStatusData::new(true)).expect("serialize status"),
        json!({ "authenticated": true })
    );
    assert_eq!(
        serde_json::to_value(AdminLogoutData::new()).expect("serialize logout"),
        json!({ "message": "Logged out successfully" })
    );
}

#[tokio::test]
async fn default_auth_service_should_initialize_login_validate_and_logout() {
    let fixture = AdminTestFixture::new().await;
    let service = fixture.services.auth();
    let session = service
        .login(LoginCommand {
            source: "127.0.0.1".to_owned(),
            username: Some("admin_1".to_owned()),
            password: "strong-admin-password".to_owned(),
        })
        .await
        .expect("login succeeds");
    assert!(
        service
            .validate_session(Some(&session.session_id))
            .await
            .expect("validate session")
    );
    assert_eq!(
        service
            .resolve_admin_user_id(Some(&session.session_id))
            .await
            .expect("resolve session")
            .as_deref(),
        Some("admin_1")
    );
    service
        .logout(&session.session_id)
        .await
        .expect("logout session");
    assert!(
        !service
            .validate_session(Some(&session.session_id))
            .await
            .expect("validate logged-out session")
    );
    assert_eq!(fixture.auth.audit_count(), 2);
}

#[tokio::test]
async fn default_auth_service_should_throttle_repeated_invalid_identity() {
    let fixture = AdminTestFixture::new().await;
    for attempt in 1..=5 {
        let error = fixture
            .services
            .auth()
            .login(LoginCommand {
                source: "shared-source".to_owned(),
                username: Some("wrong-user".to_owned()),
                password: "wrong-password".to_owned(),
            })
            .await
            .expect_err("invalid login");
        if attempt < 5 {
            assert_eq!(error, LoginError::InvalidCredentials);
        } else {
            assert_eq!(error, LoginError::Throttled);
        }
    }
    assert_eq!(
        fixture
            .services
            .auth()
            .login(LoginCommand {
                source: "shared-source".to_owned(),
                username: None,
                password: "anything".to_owned(),
            })
            .await
            .expect_err("source remains throttled"),
        LoginError::Throttled
    );
    assert_eq!(fixture.auth.session_count(), 0);
}

#[tokio::test]
async fn default_auth_service_should_verify_only_full_plaintext_admin_key() {
    let fixture = AdminTestFixture::new().await;
    let key = format!("admin-{}", "a".repeat(64));
    fixture.auth.set_api_key(&key);

    assert!(
        fixture
            .services
            .auth()
            .verify_admin_api_key(&key)
            .await
            .unwrap()
    );
    assert!(
        !fixture
            .services
            .auth()
            .verify_admin_api_key("admin-short")
            .await
            .unwrap()
    );
    assert!(
        !fixture
            .services
            .auth()
            .verify_admin_api_key(&format!("admin-{}", "b".repeat(64)))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn audit_failure_should_revoke_new_session_before_returning_it() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.fail_audit(true);

    assert_eq!(
        fixture
            .services
            .auth()
            .login(LoginCommand {
                source: "source".to_owned(),
                username: None,
                password: "strong-admin-password".to_owned(),
            })
            .await
            .expect_err("audit failure rejects login"),
        LoginError::Unavailable
    );
    assert_eq!(fixture.auth.session_count(), 0);
}

#[tokio::test]
async fn login_route_should_bucket_throttling_by_client_ip() {
    use std::net::SocketAddr;

    use axum::{body::Body, extract::ConnectInfo, http::Request};
    use tower::ServiceExt as _;

    #[derive(Clone)]
    struct AuthState(gateway_admin::AdminServices);

    impl gateway_api::admin::AdminSessionState for AuthState {
        fn admin_services(&self) -> &gateway_admin::AdminServices {
            &self.0
        }
    }

    let fixture = AdminTestFixture::new().await;
    let app = gateway_api::admin::auth::router::<AuthState>()
        .with_state(AuthState(fixture.services.clone()));

    let failed_login_from = |ip: [u8; 4]| {
        let app = app.clone();
        async move {
            let mut request = Request::builder()
                .method("POST")
                .uri("/api/admin/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"username":"admin_1","password":"wrong-password"}"#,
                ))
                .expect("build login request");
            request
                .extensions_mut()
                .insert(ConnectInfo(SocketAddr::from((ip, 50_000))));
            app.oneshot(request).await.expect("login response").status()
        }
    };

    for _ in 0..4 {
        assert_eq!(
            failed_login_from([10, 0, 0, 1]).await,
            axum::http::StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        failed_login_from([10, 0, 0, 1]).await,
        axum::http::StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        failed_login_from([10, 0, 0, 2]).await,
        axum::http::StatusCode::UNAUTHORIZED
    );
}
