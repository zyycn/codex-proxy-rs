use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use gateway_api::admin::AdminServiceError;
use gateway_api::admin::auth::{
    AdminAuthAuditEvent, AdminAuthBackend, AdminAuthService, AdminBackendSession, AdminLoginData,
    AdminLoginError, AdminLoginRequest, AdminLogoutData, AdminSessionResolver,
    AdminSessionStatusData, DefaultAdminAuthService,
};
use serde_json::json;

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

#[derive(Default)]
struct MemoryAuthBackend {
    password_hash: Mutex<Option<String>>,
    admin_api_key: Mutex<Option<String>>,
    sessions: Mutex<BTreeMap<String, AdminBackendSession>>,
    login_failures: Mutex<BTreeMap<String, u32>>,
    audits: Mutex<Vec<AdminAuthAuditEvent>>,
    fail_audit: AtomicBool,
}

#[async_trait]
impl AdminAuthBackend for MemoryAuthBackend {
    async fn password_hash(
        &self,
        _admin_user_id: &str,
    ) -> Result<Option<String>, AdminServiceError> {
        Ok(self.password_hash.lock().expect("password lock").clone())
    }

    async fn store_password_hash(
        &self,
        _admin_user_id: &str,
        password_hash: &str,
    ) -> Result<(), AdminServiceError> {
        *self.password_hash.lock().expect("password lock") = Some(password_hash.to_owned());
        Ok(())
    }

    async fn admin_api_key(&self) -> Result<Option<String>, AdminServiceError> {
        Ok(self.admin_api_key.lock().expect("API key lock").clone())
    }

    async fn load_admin_session(
        &self,
        session_id: &str,
    ) -> Result<Option<AdminBackendSession>, AdminServiceError> {
        Ok(self
            .sessions
            .lock()
            .expect("session lock")
            .get(session_id)
            .cloned())
    }

    async fn store_admin_session(
        &self,
        session_id: &str,
        session: &AdminBackendSession,
    ) -> Result<(), AdminServiceError> {
        self.sessions
            .lock()
            .expect("session lock")
            .insert(session_id.to_owned(), session.clone());
        Ok(())
    }

    async fn delete_admin_session(
        &self,
        session_id: &str,
    ) -> Result<Option<AdminBackendSession>, AdminServiceError> {
        Ok(self
            .sessions
            .lock()
            .expect("session lock")
            .remove(session_id))
    }

    async fn login_source_is_throttled(
        &self,
        source: &str,
        failure_limit: u32,
        _window_seconds: u64,
    ) -> Result<bool, AdminServiceError> {
        Ok(self
            .login_failures
            .lock()
            .expect("failure lock")
            .get(source)
            .is_some_and(|count| *count >= failure_limit))
    }

    async fn record_login_failure(
        &self,
        source: &str,
        failure_limit: u32,
        _window_seconds: u64,
    ) -> Result<bool, AdminServiceError> {
        let mut failures = self.login_failures.lock().expect("failure lock");
        let count = failures.entry(source.to_owned()).or_default();
        *count = count.saturating_add(1);
        Ok(*count >= failure_limit)
    }

    async fn clear_login_failures(&self, source: &str) -> Result<(), AdminServiceError> {
        self.login_failures
            .lock()
            .expect("failure lock")
            .remove(source);
        Ok(())
    }

    async fn append_auth_audit(&self, event: AdminAuthAuditEvent) -> Result<(), AdminServiceError> {
        if self.fail_audit.load(Ordering::SeqCst) {
            return Err(AdminServiceError::unavailable("audit unavailable"));
        }
        self.audits.lock().expect("audit lock").push(event);
        Ok(())
    }
}

#[tokio::test]
async fn default_auth_service_should_initialize_login_validate_and_logout() {
    let backend = Arc::new(MemoryAuthBackend::default());
    let service = DefaultAdminAuthService::new("admin_1".to_owned(), 60, backend.clone());
    assert!(
        service
            .ensure_default_admin("strong-admin-password")
            .await
            .unwrap()
    );
    assert!(
        !service
            .ensure_default_admin("different-password")
            .await
            .unwrap()
    );

    let session = service
        .login(
            "127.0.0.1",
            Some("admin_1".to_owned()),
            "strong-admin-password".to_owned(),
        )
        .await
        .expect("login succeeds");
    assert!(
        service
            .validate(Some(session.session_id.clone()))
            .await
            .unwrap()
    );
    assert_eq!(
        service
            .resolve_admin_user_id(Some(&session.session_id))
            .await
            .unwrap()
            .as_deref(),
        Some("admin_1"),
    );
    service.logout(session.session_id.clone()).await.unwrap();
    assert!(!service.validate(Some(session.session_id)).await.unwrap());
    assert_eq!(backend.audits.lock().expect("audit lock").len(), 2);
}

#[tokio::test]
async fn default_auth_service_should_throttle_repeated_invalid_identity() {
    let backend = Arc::new(MemoryAuthBackend::default());
    let service = DefaultAdminAuthService::new("admin_1".to_owned(), 60, backend.clone());
    for attempt in 1..=5 {
        let error = service
            .login(
                "shared-source",
                Some("wrong-user".to_owned()),
                "wrong-password".to_owned(),
            )
            .await
            .unwrap_err();
        if attempt < 5 {
            assert_eq!(error, AdminLoginError::InvalidCredentials);
        } else {
            assert_eq!(error, AdminLoginError::Throttled);
        }
    }
    assert_eq!(
        service
            .login("shared-source", None, "anything".to_owned())
            .await
            .unwrap_err(),
        AdminLoginError::Throttled,
    );
    assert!(backend.sessions.lock().expect("session lock").is_empty());
}

#[tokio::test]
async fn default_auth_service_should_verify_only_full_plaintext_admin_key() {
    let backend = Arc::new(MemoryAuthBackend::default());
    let key = format!("admin-{}", "a".repeat(64));
    *backend.admin_api_key.lock().expect("API key lock") = Some(key.clone());
    let service = DefaultAdminAuthService::new("admin_1".to_owned(), 60, backend);

    assert!(service.verify_admin_api_key(&key).await.unwrap());
    assert!(!service.verify_admin_api_key("admin-short").await.unwrap());
    assert!(
        !service
            .verify_admin_api_key(&format!("admin-{}", "b".repeat(64)))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn audit_failure_should_revoke_new_session_before_returning_it() {
    let backend = Arc::new(MemoryAuthBackend::default());
    let service = DefaultAdminAuthService::new("admin_1".to_owned(), 60, backend.clone());
    service
        .ensure_default_admin("strong-admin-password")
        .await
        .unwrap();
    backend.fail_audit.store(true, Ordering::SeqCst);

    assert_eq!(
        service
            .login("source", None, "strong-admin-password".to_owned())
            .await
            .unwrap_err(),
        AdminLoginError::Unavailable,
    );
}
