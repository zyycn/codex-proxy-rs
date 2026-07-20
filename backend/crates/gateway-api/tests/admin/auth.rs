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
