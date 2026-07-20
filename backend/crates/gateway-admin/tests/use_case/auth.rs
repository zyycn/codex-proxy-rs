use std::{collections::BTreeMap, sync::Mutex};

use async_trait::async_trait;

use gateway_admin::{
    model::{
        auth::{AdminAuditEvent, AdminSession, LoginCommand},
        settings::AdminApiKey,
    },
    ports::store::{AdminStoreResult, AuthStore},
};

#[derive(Default)]
struct MemoryAuthStore {
    password_hash: Mutex<Option<String>>,
    sessions: Mutex<BTreeMap<String, AdminSession>>,
    audits: Mutex<Vec<AdminAuditEvent>>,
    failures: Mutex<u32>,
}

#[async_trait]
impl AuthStore for MemoryAuthStore {
    async fn load_password_hash(&self, _: &str) -> AdminStoreResult<Option<String>> {
        Ok(self.password_hash.lock().expect("password hash").clone())
    }

    async fn create_password_hash_if_absent(
        &self,
        _: &str,
        password_hash: &str,
    ) -> AdminStoreResult<bool> {
        let mut stored = self.password_hash.lock().expect("password hash");
        if stored.is_some() {
            return Ok(false);
        }
        *stored = Some(password_hash.to_owned());
        Ok(true)
    }

    async fn load_admin_api_key(&self) -> AdminStoreResult<Option<AdminApiKey>> {
        Ok(None)
    }

    async fn load_session(&self, session_id: &str) -> AdminStoreResult<Option<AdminSession>> {
        Ok(self
            .sessions
            .lock()
            .expect("sessions")
            .get(session_id)
            .cloned())
    }

    async fn store_session(
        &self,
        session_id: &str,
        session: &AdminSession,
    ) -> AdminStoreResult<()> {
        self.sessions
            .lock()
            .expect("sessions")
            .insert(session_id.to_owned(), session.clone());
        Ok(())
    }

    async fn delete_session(&self, session_id: &str) -> AdminStoreResult<Option<AdminSession>> {
        Ok(self.sessions.lock().expect("sessions").remove(session_id))
    }

    async fn login_source_is_throttled(
        &self,
        _: &str,
        failure_limit: u32,
        _: u64,
    ) -> AdminStoreResult<bool> {
        Ok(*self.failures.lock().expect("failures") >= failure_limit)
    }

    async fn record_login_failure(
        &self,
        _: &str,
        failure_limit: u32,
        _: u64,
    ) -> AdminStoreResult<bool> {
        let mut failures = self.failures.lock().expect("failures");
        *failures += 1;
        Ok(*failures >= failure_limit)
    }

    async fn clear_login_failures(&self, _: &str) -> AdminStoreResult<()> {
        *self.failures.lock().expect("failures") = 0;
        Ok(())
    }

    async fn append_audit_event(&self, event: AdminAuditEvent) -> AdminStoreResult<()> {
        self.audits.lock().expect("audits").push(event);
        Ok(())
    }
}

#[tokio::test]
async fn successful_login_should_create_expiring_session_and_audit() {
    let store = std::sync::Arc::new(MemoryAuthStore::default());
    let services = super::AdminHarness::new().auth(store.clone()).build().await;

    let result = services
        .auth()
        .login(LoginCommand {
            username: Some("admin".to_owned()),
            password: "strong-test-password".to_owned(),
            source: "127.0.0.1".to_owned(),
        })
        .await
        .expect("login");

    assert!(
        services
            .auth()
            .validate_session(Some(&result.session_id))
            .await
            .expect("validate")
    );
    assert_eq!(store.audits.lock().expect("audits").len(), 1);
}

#[tokio::test]
async fn repeated_default_initialization_should_not_replace_password() {
    let store = std::sync::Arc::new(MemoryAuthStore::default());
    super::AdminHarness::new()
        .auth(store.clone())
        .default_password("first-strong-password")
        .build()
        .await;
    let services = super::AdminHarness::new()
        .auth(store)
        .default_password("second-strong-password")
        .build()
        .await;

    assert!(
        services
            .auth()
            .login(LoginCommand {
                username: Some("admin".to_owned()),
                password: "first-strong-password".to_owned(),
                source: "127.0.0.1".to_owned(),
            })
            .await
            .is_ok()
    );
}
