//! External OIDC verifier contracts.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode, get_current_timestamp};
use p256::ecdsa::SigningKey;
use p256::pkcs8::EncodePrivateKey;
use serde::Serialize;
use serde_json::{Value, json};
use url::Url;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use provider_xai::{
    OFFICIAL_CLIENT_ID, OFFICIAL_ISSUER, ReqwestOidcTokenVerifier, SecretValue, TokenCandidate,
    TokenVerificationContext, TokenVerifier, VerificationFailure, VerificationFlow,
    VerificationMethod,
};

use crate::support::loopback_endpoint_policy;

const CACHE_TTL: Duration = Duration::from_secs(60 * 60);

#[tokio::test]
async fn authorization_code_should_validate_es256_and_cache_jwks() {
    let server = MockServer::start().await;
    let endpoint = endpoints(&server);
    let key = TestKey::new(7, "oauth-key-1");
    mount_jwks(&server, key.jwks(), 1).await;
    let verifier = verifier(&endpoint.origin, CACHE_TTL);
    let nonce = SecretValue::new("nonce-bound-to-auth-request".to_owned());
    let algorithms = vec!["ES256".to_owned()];
    let claims = valid_claims(&nonce);
    let token = SecretValue::new(key.sign(&claims, Algorithm::ES256));
    let access_token = SecretValue::new("access-token".to_owned());

    for _ in 0..2 {
        let evidence = verifier
            .verify(
                auth_context(&endpoint, &algorithms, &nonce),
                TokenCandidate::new(&access_token, Some(&token), Some(Duration::from_secs(60))),
            )
            .await
            .expect("valid ID token should cross the trust boundary");
        assert_eq!(evidence.method(), VerificationMethod::IdToken);
        assert_eq!(evidence.subject(), "user-123");
    }
}

#[tokio::test]
async fn fresh_cache_miss_should_refresh_once_for_key_rotation() {
    let server = MockServer::start().await;
    let endpoint = endpoints(&server);
    let old_key = TestKey::new(11, "old-key");
    let new_key = TestKey::new(13, "new-key");
    mount_jwks_once_then(&server, old_key.jwks(), new_key.jwks()).await;
    let verifier = verifier(&endpoint.origin, CACHE_TTL);
    let nonce = SecretValue::new("rotation-nonce".to_owned());
    let algorithms = vec!["ES256".to_owned()];
    let access_token = SecretValue::new("access-token".to_owned());
    let old_token = SecretValue::new(old_key.sign(&valid_claims(&nonce), Algorithm::ES256));
    let new_token = SecretValue::new(new_key.sign(&valid_claims(&nonce), Algorithm::ES256));

    verifier
        .verify(
            auth_context(&endpoint, &algorithms, &nonce),
            TokenCandidate::new(&access_token, Some(&old_token), None),
        )
        .await
        .expect("old key should warm cache");
    verifier
        .verify(
            auth_context(&endpoint, &algorithms, &nonce),
            TokenCandidate::new(&access_token, Some(&new_token), None),
        )
        .await
        .expect("new kid should force exactly one refresh");
}

#[tokio::test]
async fn authorization_code_claims_and_algorithm_should_fail_closed() {
    let server = MockServer::start().await;
    let endpoint = endpoints(&server);
    let key = TestKey::new(17, "validation-key");
    mount_jwks(&server, key.jwks(), 1).await;
    let verifier = verifier(&endpoint.origin, CACHE_TTL);
    let nonce = SecretValue::new("expected-nonce".to_owned());
    let algorithms = vec!["ES256".to_owned()];
    let access_token = SecretValue::new("access-token".to_owned());

    let invalid_claims = vec![
        IdTokenClaims {
            nonce: "wrong-nonce".to_owned(),
            ..valid_claims(&nonce)
        },
        IdTokenClaims {
            aud: json!("another-client"),
            ..valid_claims(&nonce)
        },
        IdTokenClaims {
            iss: "https://issuer.example".to_owned(),
            ..valid_claims(&nonce)
        },
        IdTokenClaims {
            exp: get_current_timestamp().saturating_sub(1),
            ..valid_claims(&nonce)
        },
        IdTokenClaims {
            sub: String::new(),
            ..valid_claims(&nonce)
        },
    ];

    for claims in invalid_claims {
        let token = SecretValue::new(key.sign(&claims, Algorithm::ES256));
        let error = verifier
            .verify(
                auth_context(&endpoint, &algorithms, &nonce),
                TokenCandidate::new(&access_token, Some(&token), None),
            )
            .await
            .expect_err("invalid claim must fail closed");
        assert_eq!(error, VerificationFailure::Rejected);
    }

    let token = SecretValue::new(key.sign(&valid_claims(&nonce), Algorithm::HS256));
    let error = verifier
        .verify(
            auth_context(&endpoint, &algorithms, &nonce),
            TokenCandidate::new(&access_token, Some(&token), None),
        )
        .await
        .expect_err("symmetric algorithm must be rejected before claim trust");
    assert_eq!(error, VerificationFailure::Rejected);
}

#[tokio::test]
async fn credential_import_should_use_official_userinfo() {
    let server = MockServer::start().await;
    let endpoint = endpoints(&server);
    Mock::given(method("GET"))
        .and(path("/oauth2/userinfo"))
        .and(header("authorization", "Bearer imported-access-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({"sub": "user-123", "email": "ignored@example.com"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let verifier = verifier(&endpoint.origin, CACHE_TTL);
    let algorithms = vec!["ES256".to_owned()];
    let access_token = SecretValue::new("imported-access-token".to_owned());

    let evidence = verifier
        .verify(
            import_context(&endpoint, &algorithms),
            TokenCandidate::new(&access_token, None, Some(Duration::from_secs(60))),
        )
        .await
        .expect("official userinfo verifies the access token");

    assert_eq!(evidence.method(), VerificationMethod::UserInfo);
}

#[tokio::test]
async fn credential_import_should_not_require_a_still_current_id_token() {
    let server = MockServer::start().await;
    let endpoint = endpoints(&server);
    Mock::given(method("GET"))
        .and(path("/oauth2/userinfo"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({"sub": "different-user"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let verifier = verifier(&endpoint.origin, CACHE_TTL);
    let algorithms = vec!["ES256".to_owned()];
    let stale_id_token = SecretValue::new("expired.payload.signature".to_owned());
    let access_token = SecretValue::new("imported-access-token".to_owned());

    let evidence = verifier
        .verify(
            import_context(&endpoint, &algorithms),
            TokenCandidate::new(&access_token, Some(&stale_id_token), None),
        )
        .await
        .expect("current access token is verified by official userinfo");

    assert_eq!(evidence.method(), VerificationMethod::UserInfo);
}

#[tokio::test]
async fn userinfo_status_and_endpoint_policy_should_fail_closed() {
    let server = MockServer::start().await;
    let endpoint = endpoints(&server);
    Mock::given(method("GET"))
        .and(path("/oauth2/userinfo"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    let verifier = verifier(&endpoint.origin, CACHE_TTL);
    let algorithms = vec!["ES256".to_owned()];
    let access_token = SecretValue::new("access-token".to_owned());

    let unavailable = verifier
        .verify(
            import_context(&endpoint, &algorithms),
            TokenCandidate::new(&access_token, None, None),
        )
        .await
        .expect_err("5xx userinfo response must be transient unavailable");
    assert_eq!(unavailable, VerificationFailure::Unavailable);

    let attacker = Url::parse("https://attacker.example/oauth2/userinfo").expect("test URL");
    let rejected = verifier
        .verify(
            TokenVerificationContext::new(
                VerificationFlow::CredentialImport,
                &endpoint.issuer,
                OFFICIAL_CLIENT_ID,
                &endpoint.jwks,
                &attacker,
                &algorithms,
                None,
            ),
            TokenCandidate::new(&access_token, None, None),
        )
        .await
        .expect_err("cross-origin endpoint must be rejected before I/O");
    assert_eq!(rejected, VerificationFailure::Rejected);
}

#[tokio::test]
async fn jwks_redirect_should_not_be_followed() {
    let server = MockServer::start().await;
    let endpoint = endpoints(&server);
    Mock::given(method("GET"))
        .and(path("/.well-known/jwks.json"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "https://attacker.example/.well-known/jwks.json"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let verifier = verifier(&endpoint.origin, CACHE_TTL);
    let nonce = SecretValue::new("redirect-nonce".to_owned());
    let algorithms = vec!["ES256".to_owned()];
    let access_token = SecretValue::new("access-token".to_owned());
    let key = TestKey::new(19, "redirect-key");
    let token = SecretValue::new(key.sign(&valid_claims(&nonce), Algorithm::ES256));

    let error = verifier
        .verify(
            auth_context(&endpoint, &algorithms, &nonce),
            TokenCandidate::new(&access_token, Some(&token), None),
        )
        .await
        .expect_err("redirected JWKS must be rejected");

    assert_eq!(error, VerificationFailure::Rejected);
}

struct TestEndpoints {
    origin: Url,
    issuer: Url,
    jwks: Url,
    userinfo: Url,
}

fn endpoints(server: &MockServer) -> TestEndpoints {
    let origin = Url::parse(&format!("{}/", server.uri())).expect("wiremock origin");
    TestEndpoints {
        issuer: Url::parse(OFFICIAL_ISSUER).expect("official issuer"),
        jwks: origin
            .join(".well-known/jwks.json")
            .expect("wiremock JWKS URL"),
        userinfo: origin
            .join("oauth2/userinfo")
            .expect("wiremock userinfo URL"),
        origin,
    }
}

fn verifier(origin: &Url, cache_ttl: Duration) -> ReqwestOidcTokenVerifier {
    ReqwestOidcTokenVerifier::new(loopback_endpoint_policy(origin), cache_ttl)
        .expect("loopback verifier")
}

fn auth_context<'a>(
    endpoints: &'a TestEndpoints,
    algorithms: &'a [String],
    nonce: &'a SecretValue,
) -> TokenVerificationContext<'a> {
    TokenVerificationContext::new(
        VerificationFlow::AuthorizationCode,
        &endpoints.issuer,
        OFFICIAL_CLIENT_ID,
        &endpoints.jwks,
        &endpoints.userinfo,
        algorithms,
        Some(nonce),
    )
}

fn import_context<'a>(
    endpoints: &'a TestEndpoints,
    algorithms: &'a [String],
) -> TokenVerificationContext<'a> {
    TokenVerificationContext::new(
        VerificationFlow::CredentialImport,
        &endpoints.issuer,
        OFFICIAL_CLIENT_ID,
        &endpoints.jwks,
        &endpoints.userinfo,
        algorithms,
        None,
    )
}

async fn mount_jwks(server: &MockServer, body: Value, expected: u64) {
    Mock::given(method("GET"))
        .and(path("/.well-known/jwks.json"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(body),
        )
        .expect(expected)
        .mount(server)
        .await;
}

async fn mount_jwks_once_then(server: &MockServer, first: Value, second: Value) {
    Mock::given(method("GET"))
        .and(path("/.well-known/jwks.json"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(first),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/jwks.json"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(second),
        )
        .expect(1)
        .mount(server)
        .await;
}

#[derive(Serialize)]
struct IdTokenClaims {
    iss: String,
    aud: Value,
    exp: u64,
    sub: String,
    nonce: String,
}

fn valid_claims(nonce: &SecretValue) -> IdTokenClaims {
    IdTokenClaims {
        iss: OFFICIAL_ISSUER.to_owned(),
        aud: json!(OFFICIAL_CLIENT_ID),
        exp: get_current_timestamp() + 300,
        sub: "user-123".to_owned(),
        nonce: nonce.expose().to_owned(),
    }
}

struct TestKey {
    kid: &'static str,
    signing_key: SigningKey,
    x: String,
    y: String,
}

impl TestKey {
    fn new(seed: u8, kid: &'static str) -> Self {
        let signing_key = SigningKey::from_slice(&[seed; 32]).expect("valid deterministic scalar");
        let point = signing_key.verifying_key().to_encoded_point(false);
        let x = URL_SAFE_NO_PAD.encode(point.x().expect("P-256 x coordinate"));
        let y = URL_SAFE_NO_PAD.encode(point.y().expect("P-256 y coordinate"));
        Self {
            kid,
            signing_key,
            x,
            y,
        }
    }

    fn jwks(&self) -> Value {
        json!({
            "keys": [{
                "kty": "EC",
                "use": "sig",
                "crv": "P-256",
                "kid": self.kid,
                "x": self.x,
                "y": self.y,
                "alg": "ES256"
            }]
        })
    }

    fn sign(&self, claims: &IdTokenClaims, algorithm: Algorithm) -> String {
        let mut header = Header::new(algorithm);
        header.kid = Some(self.kid.to_owned());
        let encoding_key = if algorithm == Algorithm::ES256 {
            let der = self
                .signing_key
                .to_pkcs8_der()
                .expect("encode deterministic EC key");
            EncodingKey::from_ec_der(der.as_bytes())
        } else {
            EncodingKey::from_secret(b"symmetric-test-key")
        };
        encode(&header, claims, &encoding_key).expect("encode test token")
    }
}
