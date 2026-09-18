use gateway_admin::model::auth::{LoginCommand, LoginError};
use gateway_api::auth::LoginRequest;
use serde_json::json;

use super::AdminTestFixture;

#[test]
fn password_change_request_rejects_unknown_fields_and_redacts_both_passwords() {
    let request: gateway_api::auth::ChangePasswordRequest = serde_json::from_value(json!({
        "currentPassword": "current-password-secret", "newPassword": "new-password-secret"
    }))
    .unwrap();
    assert!(!format!("{request:?}").contains("password-secret"));
    assert!(
        serde_json::from_value::<gateway_api::auth::ChangePasswordRequest>(json!({
            "currentPassword": "old", "newPassword": "new", "username": "another-admin"
        }))
        .is_err()
    );
}

#[test]
fn login_request_should_deny_unknown_fields_and_redact_password_debug() {
    let password = "admin-password-must-not-leak";
    let request = serde_json::from_value::<LoginRequest>(json!({
        "mode": "admin",
        "username": "admin@example.invalid",
        "password": password
    }))
    .expect("deserialize login request");

    assert!(!format!("{request:?}").contains(password));
    let LoginCommand::Admin {
        username,
        password: parsed_password,
    } = request.into()
    else {
        panic!("admin login expected")
    };
    assert_eq!(username.as_deref(), Some("admin@example.invalid"));
    assert_eq!(parsed_password, password);
    assert!(
        serde_json::from_value::<LoginRequest>(json!({
        "mode": "admin",
            "password": password,
            "rememberMe": true
        }))
        .is_err()
    );
}

#[tokio::test]
async fn default_auth_service_should_initialize_login_validate_and_logout() {
    let fixture = AdminTestFixture::new().await;
    let service = fixture.services.auth();
    let session = service
        .login(
            LoginCommand::Admin {
                username: Some("admin_1".to_owned()),
                password: "strong-admin-password".to_owned(),
            },
            std::net::Ipv4Addr::LOCALHOST.into(),
            None,
        )
        .await
        .expect("login succeeds");
    assert!(
        service
            .session(Some(&session.session_id))
            .await
            .expect("validate session")
            .is_some()
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
            .session(Some(&session.session_id))
            .await
            .expect("validate logged-out session")
            .is_some()
    );
    assert_eq!(fixture.auth.audit_count(), 2);
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
            .login(
                LoginCommand::Admin {
                    username: None,
                    password: "strong-admin-password".to_owned(),
                },
                std::net::Ipv4Addr::LOCALHOST.into(),
                None
            )
            .await
            .expect_err("audit failure rejects login"),
        LoginError::Unavailable
    );
    assert_eq!(fixture.auth.session_count(), 0);
}
