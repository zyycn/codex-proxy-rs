use codex_proxy_rs::accounts::oauth::{OAuthConfig, PkceSessionStore};

#[test]
fn pkce_session_store_should_build_login_url_and_acquire_session() {
    let mut store = PkceSessionStore::default();
    let config = OAuthConfig {
        client_id: "codex-client".to_string(),
        auth_endpoint: "https://auth.openai.com/oauth/authorize".to_string(),
        device_code_endpoint: "https://auth.openai.com/oauth/device/code".to_string(),
        token_endpoint: "https://auth.openai.com/oauth/token".to_string(),
    };

    let login = store.start_login("console.example.com", &config);
    let session = store
        .try_acquire(&login.state)
        .expect("pkce session should be available");

    assert!(login
        .auth_url
        .starts_with("https://auth.openai.com/oauth/authorize?"));
    assert!(login.auth_url.contains("response_type=code"));
    assert!(login.auth_url.contains("client_id=codex-client"));
    assert!(login
        .auth_url
        .contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
    assert!(login
        .auth_url
        .contains("scope=openid%20profile%20email%20offline_access"));
    assert!(login.auth_url.contains(&format!("state={}", login.state)));
    assert_eq!(session.redirect_uri, "http://localhost:1455/auth/callback");
    assert_eq!(session.return_host, "console.example.com");
    assert!(!session.code_verifier.is_empty());
}

#[test]
fn pkce_session_store_should_treat_acquired_and_completed_states_as_unavailable() {
    let mut store = PkceSessionStore::default();
    let config = OAuthConfig {
        client_id: "codex-client".to_string(),
        auth_endpoint: "https://auth.openai.com/oauth/authorize".to_string(),
        device_code_endpoint: "https://auth.openai.com/oauth/device/code".to_string(),
        token_endpoint: "https://auth.openai.com/oauth/token".to_string(),
    };

    let login = store.start_login("console.example.com", &config);
    let _session = store
        .try_acquire(&login.state)
        .expect("first acquire should succeed");

    assert!(store.try_acquire(&login.state).is_none());
    assert!(store.is_completed_or_exchanging(&login.state));

    store.release(&login.state);
    assert!(store.try_acquire(&login.state).is_some());

    store.complete(&login.state);
    assert!(store.try_acquire(&login.state).is_none());
    assert!(store.is_completed_or_exchanging(&login.state));
}
