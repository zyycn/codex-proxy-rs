//! 验证双凭据续期、响应重试、并发撤销与固定期限边界

use super::super::AdminHarness;
use super::{MemoryAuthStore, admin_login, stored_id};
use chrono::{TimeDelta, Utc};
use gateway_admin::model::auth::AuthSession;

#[tokio::test]
async fn expired_access_is_restored_by_refresh_and_duplicate_refresh_replays_the_same_grant() {
    let store = std::sync::Arc::new(MemoryAuthStore::default());
    let services = AdminHarness::new().auth(store.clone()).build().await;
    let auth = services.auth();
    let login = admin_login(auth).await;
    let refresh = login.refresh_token.as_deref().unwrap();
    assert!(auth.session(Some(refresh)).await.unwrap().is_none());
    assert!(
        auth.renew_session(Some(&login.session_id), None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        auth.renew_session(None, Some(&login.session_id))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        auth.renew_session(Some(refresh), None).await.unwrap(),
        Some(login.clone())
    );
    store
        .sessions
        .lock()
        .unwrap()
        .get_mut(stored_id(&login.session_id))
        .unwrap()
        .expires_at = Utc::now() - TimeDelta::seconds(1);
    assert!(
        auth.session(Some(&login.session_id))
            .await
            .unwrap()
            .is_none()
    );
    let (first, concurrent) = tokio::join!(
        auth.renew_session(Some(refresh), None),
        auth.renew_session(Some(refresh), None)
    );
    let first = first.unwrap().unwrap();
    assert_eq!(concurrent.unwrap().as_ref(), Some(&first));
    assert_ne!(first.session_id, login.session_id);
    assert_ne!(first.refresh_token, login.refresh_token);
    assert_eq!(
        first.session.absolute_expires_at,
        login.session.absolute_expires_at
    );
    assert!(
        auth.session(Some(&first.session_id))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        auth.renew_session(first.refresh_token.as_deref(), None)
            .await
            .unwrap(),
        Some(first.clone())
    );
    assert_eq!(
        auth.renew_session(Some(refresh), None).await.unwrap(),
        Some(first.clone())
    );
    let persisted = format!("{:?}", store.sessions.lock().unwrap());
    for token in [
        &first.session_id,
        first.refresh_token.as_ref().unwrap(),
        &login.session_id,
        login.refresh_token.as_ref().unwrap(),
    ] {
        assert!(!persisted.contains(token));
        assert!(!persisted.contains(token.rsplit('.').next().unwrap()));
    }
}

#[tokio::test]
async fn late_refresh_reuse_revokes_the_family_but_unknown_secrets_do_not() {
    let store = std::sync::Arc::new(MemoryAuthStore::default());
    let services = AdminHarness::new().auth(store.clone()).build().await;
    let auth = services.auth();
    let login = admin_login(auth).await;
    store
        .sessions
        .lock()
        .unwrap()
        .get_mut(stored_id(&login.session_id))
        .unwrap()
        .expires_at = Utc::now() - TimeDelta::seconds(1);
    let next = auth
        .renew_session(login.refresh_token.as_deref(), None)
        .await
        .unwrap()
        .unwrap();
    let unknown = format!("r2.{}.{}", stored_id(&login.session_id), "A".repeat(43));
    assert!(
        auth.renew_session(Some(&unknown), None)
            .await
            .unwrap()
            .is_none()
    );
    auth.logout(&unknown).await.unwrap();
    auth.logout(stored_id(&login.session_id)).await.unwrap();
    assert!(
        auth.session(Some(&next.session_id))
            .await
            .unwrap()
            .is_some()
    );
    store
        .sessions
        .lock()
        .unwrap()
        .get_mut(stored_id(&login.session_id))
        .unwrap()
        .renewal
        .as_mut()
        .unwrap()
        .previous
        .as_mut()
        .unwrap()
        .expires_at = Utc::now() - TimeDelta::seconds(1);
    assert!(
        auth.renew_session(login.refresh_token.as_deref(), None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        auth.session(Some(&next.session_id))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        auth.renew_session(next.refresh_token.as_deref(), None)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn refresh_respects_idle_and_absolute_expiry_and_logout_after_access_expiry() {
    let store = std::sync::Arc::new(MemoryAuthStore::default());
    let services = AdminHarness::new().auth(store.clone()).build().await;
    let auth = services.auth();
    for absolute in [false, true] {
        let login = admin_login(auth).await;
        {
            let mut sessions = store.sessions.lock().unwrap();
            let session = sessions.get_mut(stored_id(&login.session_id)).unwrap();
            if absolute {
                session.absolute_expires_at = Some(Utc::now() - TimeDelta::seconds(1));
            } else {
                session.renewal.as_mut().unwrap().expires_at = Utc::now() - TimeDelta::seconds(1);
            }
        }
        assert!(
            auth.renew_session(login.refresh_token.as_deref(), None)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            auth.session(Some(&login.session_id))
                .await
                .unwrap()
                .is_none()
        );
    }
    let login = admin_login(auth).await;
    store
        .sessions
        .lock()
        .unwrap()
        .get_mut(stored_id(&login.session_id))
        .unwrap()
        .expires_at = Utc::now() - TimeDelta::seconds(1);
    auth.logout(login.refresh_token.as_deref().unwrap())
        .await
        .unwrap();
    assert!(
        auth.renew_session(login.refresh_token.as_deref(), None)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn refreshed_expiry_is_capped_and_store_outages_preserve_the_session() {
    let store = std::sync::Arc::new(MemoryAuthStore::default());
    let services = AdminHarness::new().auth(store.clone()).build().await;
    let auth = services.auth();
    let login = admin_login(auth).await;
    let limit = Utc::now() + TimeDelta::minutes(1);
    {
        let mut sessions = store.sessions.lock().unwrap();
        let session = sessions.get_mut(stored_id(&login.session_id)).unwrap();
        session.absolute_expires_at = Some(limit);
        session.expires_at = Utc::now() - TimeDelta::seconds(1);
    }
    store
        .unavailable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        auth.renew_session(login.refresh_token.as_deref(), None)
            .await
            .is_err()
    );
    store
        .unavailable
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let refreshed = auth
        .renew_session(login.refresh_token.as_deref(), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refreshed.session.expires_at, limit);
    assert_eq!(refreshed.session.retained_until(), limit);
    assert_eq!(refreshed.session.absolute_expires_at, Some(limit));
}

#[tokio::test]
async fn fixed_sessions_cannot_acquire_refresh_credentials() {
    let store = std::sync::Arc::new(MemoryAuthStore::default());
    let services = AdminHarness::new().auth(store.clone()).build().await;
    let auth = services.auth();
    let login = admin_login(auth).await;
    let fixed = AuthSession {
        renewal: None,
        ..login.session
    };
    store
        .sessions
        .lock()
        .unwrap()
        .insert("fixed".into(), fixed.clone());
    let grant = auth
        .renew_session(None, Some("fixed"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(grant.session, fixed);
    assert!(grant.refresh_token.is_none());
    store
        .sessions
        .lock()
        .unwrap()
        .get_mut("fixed")
        .unwrap()
        .expires_at = Utc::now() - TimeDelta::seconds(1);
    assert!(
        auth.renew_session(None, Some("fixed"))
            .await
            .unwrap()
            .is_none()
    );
}
