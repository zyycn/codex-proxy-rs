//! 为原生传输回归准备经过真实授权交换和旋转刷新的测试凭据

use super::oauth::{GoogleOAuth, OAuthRegistration};
use serde_json::json;
use url::Url;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

pub(crate) async fn refreshed_credential(server: &MockServer) -> (Url, String) {
    let origin = server.uri();
    let profile = crate::profile::Selection::default().resolve("2.34.0");
    let config:OAuthRegistration=serde_json::from_value(json!({"client_id":"fixture-cpr","client_secret":"fixture-secret","redirect_uri":"http://127.0.0.1:8020/callback","resource_scopes":["fixture-resource"],"resource_origin":origin})).unwrap();
    config.validate().unwrap();
    let oauth = GoogleOAuth::with_endpoints(
        config,
        &format!("{origin}/token"),
        &format!("{origin}/userinfo"),
        &format!("{origin}/tokeninfo"),
    );
    let link = oauth.authorize_url("fixture-state", "verifier").unwrap();
    assert!(link.contains("code_challenge_method=S256"));
    let code = oauth
        .callback_code(
            "http://127.0.0.1:8020/callback?code=fixture-code&state=fixture-state",
            "fixture-state",
        )
        .unwrap();
    let token = |access, refresh| {
        ResponseTemplate::new(200).set_body_json(json!({"token_type":"Bearer","access_token":access,"refresh_token":refresh,"expires_in":3600,"scope":"openid email fixture-resource"}))
    };
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(|r: &wiremock::Request| {
            String::from_utf8_lossy(&r.body).contains("grant_type=authorization_code")
        })
        .respond_with(token("initial-access", "initial-refresh"))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/userinfo"))
        .and(header("authorization", "Bearer initial-access"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"id":"user-1","email":"test@example.org","verified_email":true}),
            ),
        )
        .expect(1)
        .mount(server)
        .await;
    let grant = oauth
        .exchange(&code, "verifier", None, &profile)
        .await
        .unwrap();
    assert_eq!(grant.email.as_deref(), Some("test@example.org"));
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(|r: &wiremock::Request| {
            String::from_utf8_lossy(&r.body).contains("refresh_token=initial-refresh")
        })
        .respond_with(token("renewed-access", "rotated-refresh"))
        .expect(1)
        .mount(server)
        .await;
    let updated = oauth
        .refresh(&grant.material, None, &profile)
        .await
        .unwrap();
    assert_eq!(updated.material.refresh_token, "rotated-refresh");
    (
        oauth.resource_origin().clone(),
        updated.material.access_token,
    )
}
