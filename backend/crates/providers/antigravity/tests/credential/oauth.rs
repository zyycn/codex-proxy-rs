//! 验证 OAuth 注册、PKCE、回调与离线授权边界

use super::oauth::{self, GoogleOAuth, OAuthRegistration};
use serde_json::json;
use url::Url;

fn registration() -> serde_json::Value {
    json!({"client_id":"fixture-client","client_secret":"fixture-secret","redirect_uri":"http://127.0.0.1:8020/callback","resource_scopes":["fixture-resource"],"resource_origin":"https://resource.example/"})
}

#[test]
fn authorization_link_separates_secret_state_and_pkce() {
    let config: OAuthRegistration = serde_json::from_value(registration()).unwrap();
    assert!(config.validate().is_ok());
    assert!(!format!("{config:?}").contains("fixture-secret"));
    let oauth = GoogleOAuth::for_provider().unwrap();
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let url = Url::parse(&oauth.authorize_url("state", verifier).unwrap()).unwrap();
    let fields = url
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(url.host_str(), Some("accounts.google.com"));
    assert_eq!(url.path(), "/o/oauth2/auth");
    assert_eq!(
        fields["client_id"],
        "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com"
    );
    assert_eq!(
        fields["redirect_uri"],
        "http://localhost:8085/auth/callback"
    );
    assert_eq!(fields["code_challenge_method"], "S256");
    assert_eq!(fields["response_type"], "code");
    assert_eq!(fields["access_type"], "offline");
    assert_eq!(fields["prompt"], "consent");
    assert_eq!(
        fields["code_challenge"],
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    assert_eq!(fields["state"], "state");
    assert_eq!(
        fields["scope"]
            .split_ascii_whitespace()
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "https://www.googleapis.com/auth/userinfo.email",
            "https://www.googleapis.com/auth/userinfo.profile",
            "https://www.googleapis.com/auth/cloud-platform",
            "https://www.googleapis.com/auth/cclog",
            "https://www.googleapis.com/auth/experimentsandconfigs",
            "https://www.googleapis.com/auth/aicode",
        ]
        .into()
    );
    assert!(!fields.contains_key("client_secret") && !url.as_str().contains(verifier));
    assert_eq!(
        oauth.resource_origin().as_str(),
        "https://daily-cloudcode-pa.googleapis.com/"
    );
    assert_eq!(
        oauth
            .callback_code(
                "http://localhost:8085/auth/callback?state=state&code=code",
                "state"
            )
            .unwrap(),
        "code"
    );
}

#[tokio::test]
async fn exchange_and_refresh_preserve_verified_identity_and_offline_grant() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, header, method, path},
    };

    let server = MockServer::start().await;
    let profile = crate::profile::Selection::default().resolve("2.34.0");
    let oauth = GoogleOAuth::with_endpoints(
        serde_json::from_value(registration()).unwrap(),
        &format!("{}/token", server.uri()),
        &format!("{}/userinfo", server.uri()),
        &format!("{}/tokeninfo", server.uri()),
    );
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(header("user-agent", profile.user_agent()))
        .and(body_string_contains("client_id=fixture-client"))
        .and(body_string_contains("client_secret=fixture-secret"))
        .and(body_string_contains("code=code"))
        .and(body_string_contains("code_verifier=verifier"))
        .and(body_string_contains(
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A8020%2Fcallback",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access", "refresh_token": "refresh", "token_type": "Bearer",
            "expires_in": 3600, "scope": "fixture-resource"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/userinfo"))
        .and(header("authorization", "Bearer access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "subject", "email": "account@example.com", "verified_email": true
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(header("user-agent", profile.user_agent()))
        .and(body_string_contains("client_id=fixture-client"))
        .and(body_string_contains("client_secret=fixture-secret"))
        .and(body_string_contains("refresh_token=refresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new-access", "refresh_token": "new-refresh",
            "token_type": "Bearer", "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    let grant = oauth
        .exchange("code", "verifier", None, &profile)
        .await
        .unwrap();
    assert_eq!(grant.email.as_deref(), Some("account@example.com"));
    assert_eq!(grant.material.subject, "subject");
    assert_eq!(grant.material.registration, oauth.registration());
    let refreshed = oauth
        .refresh(&grant.material, None, &profile)
        .await
        .unwrap();
    assert_eq!(refreshed.material.subject, grant.material.subject);
    assert_eq!(refreshed.material.scopes, grant.material.scopes);
    assert_eq!(refreshed.material.access_token, "new-access");
    assert_eq!(refreshed.material.refresh_token, "new-refresh");
    assert!(refreshed.expires_at > chrono::Utc::now());
    server.verify().await;
}

#[test]
fn callback_requires_exact_redirect_and_one_pair() {
    let oauth = GoogleOAuth::new(serde_json::from_value(registration()).unwrap());
    let base = "http://127.0.0.1:8020/callback";
    assert_eq!(
        oauth
            .callback_code(&format!("{base}?state=s&code=c"), "s")
            .unwrap(),
        "c"
    );
    for wrong in [
        "?code=c",
        "?state=wrong&code=c",
        "?state=s&code=c&code=b",
        "?state=s&code=c&state=s",
        "?error=access_denied&code=c&state=s",
        "?state=s&code=c#fragment",
        "/other?state=s&code=c",
    ] {
        assert!(oauth.callback_code(&format!("{base}{wrong}"), "s").is_err());
    }
    assert!(
        oauth
            .callback_code("http://127.0.0.1:8021/callback?state=s&code=c", "s")
            .is_err()
    );
    assert!(oauth.callback_code("c", "s").is_err());
}

#[test]
fn registration_fences_client_scope_and_redirect_while_allowing_secret_rotation() {
    let initial = registration();
    let fingerprint = |v| GoogleOAuth::new(serde_json::from_value(v).unwrap()).registration();
    let expected = fingerprint(initial.clone());
    for (key, value) in [
        ("client_id", json!("second")),
        ("resource_origin", json!("https://another.example/")),
        ("resource_scopes", json!(["second"])),
        ("redirect_uri", json!("https://example.org/callback")),
    ] {
        let mut config = initial.clone();
        config[key] = value;
        assert_ne!(expected, fingerprint(config));
    }
    let mut rotated = initial;
    rotated["client_secret"] = json!("new-secret");
    assert_eq!(expected, fingerprint(rotated));
}

#[test]
fn invalid_registration_cannot_request_identity_only_or_use_unsafe_redirect() {
    for (key, value) in [
        ("resource_scopes", json!(["openid", "email"])),
        (
            "resource_scopes",
            json!(["https://www.googleapis.com/auth/userinfo.email"]),
        ),
        ("resource_scopes", json!([])),
        ("client_secret", json!("")),
        ("resource_origin", json!("http://example.org/")),
        ("redirect_uri", json!("http://example.org/callback")),
        ("redirect_uri", json!("https://user@example.org/callback")),
        (
            "redirect_uri",
            json!("http://127.0.0.1/callback?something=secret"),
        ),
    ] {
        let mut v = registration();
        v[key] = value;
        let config: OAuthRegistration = serde_json::from_value(v).unwrap();
        assert!(config.validate().is_err(), "{key}");
    }
}

#[test]
fn new_grants_require_offline_access_and_explicit_resource_scope() {
    let required = vec!["fixture-resource".to_owned()];
    let response = json!({"access_token":"access","refresh_token":"refresh","token_type":"Bearer","expires_in":3600,"scope":"fixture-resource"});
    let mut token: oauth::TokenResponse = serde_json::from_value(response.clone()).unwrap();
    assert_eq!(token.resolve_grant(&required, None).unwrap().1, "refresh");
    for (key, value) in [
        ("refresh_token", json!(null)),
        ("scope", json!(null)),
        ("scope", json!("openid email")),
        ("expires_in", json!(0)),
        ("token_type", json!("unknown")),
    ] {
        let mut input = response.clone();
        input[key] = value;
        let mut token: oauth::TokenResponse = serde_json::from_value(input).unwrap();
        assert!(token.resolve_grant(&required, None).is_err(), "{key}");
    }
}

#[test]
fn refresh_handles_rotation_inheritance_and_reduced_grants() {
    let previous:oauth::Material=serde_json::from_value(json!({"registration":"client","subject":"sub","access_token":"old-at","refresh_token":"old-rt","scopes":["openid","fixture-resource"]})).unwrap();
    let required = vec!["fixture-resource".to_owned()];
    let response = json!({"access_token":"new-at","expires_in":3600,"token_type":"bearer"});
    let mut token: oauth::TokenResponse = serde_json::from_value(response.clone()).unwrap();
    let (scopes, rt) = token.resolve_grant(&required, Some(&previous)).unwrap();
    assert_eq!(scopes, previous.scopes);
    assert_eq!(rt, "old-rt");
    let mut input = response;
    input["refresh_token"] = json!("new-rt");
    let mut token: oauth::TokenResponse = serde_json::from_value(input.clone()).unwrap();
    assert_eq!(
        token.resolve_grant(&required, Some(&previous)).unwrap().1,
        "new-rt"
    );
    input["scope"] = json!("openid email");
    let mut token: oauth::TokenResponse = serde_json::from_value(input).unwrap();
    assert_eq!(
        token
            .resolve_grant(&required, Some(&previous))
            .unwrap_err()
            .kind(),
        gateway_admin::ports::provider::ProviderAdminErrorKind::CredentialRefreshRequired
    );
}
