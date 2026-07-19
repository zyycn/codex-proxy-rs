use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode, get_current_timestamp};
use provider_openai::credential::token_client::OFFICIAL_CODEX_OAUTH_CLIENT_ID;
use provider_openai::credential::{
    CodexAuthorizationTokenVerifier, CodexIdentityVerificationError, CodexJwksSource,
    CodexJwtIdentityVerifier, CodexOAuthSecret, CodexTokenIdentityVerifier,
    OFFICIAL_OPENAI_API_AUDIENCE, OFFICIAL_OPENAI_ISSUER, ReqwestOpenAiJwksSource,
};
use rsa::RsaPrivateKey;
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::traits::PublicKeyParts;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Value, json};

const KEY_ID: &str = "codex-test-key";
const PRIVATE_KEY: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQCnoamNHzFtYkXZ
frcIkvTZ9McGZ+F8QZKpyRRBoBPspi5vD3leGnBD5XYhBwybwr5yTHiFN3UJ+o+P
PTGBnMVHGezYEP+VRZhrMOuw0hJbbdjbmuGDwagLlp32ax0mUQBGDIQUDZZDGoqD
0oOlXxpmZU5JSRSO6MXtU+KNDreiA3NLECEE9ty1IjOFwkCk7opX3uPg6LOHKdbE
m0LOYISzFW+gtU05dYtMnQlbeiCzt1jxSd/c9ORyLtBCGyUlFJoK7L/odak/5n3h
rJ8rKjyeHPQ9Cxe9OXfL/wBG3A5P2zLCat3yeRNnWhuLlALcNRkse16ciT76skuG
8afSSV6/AgMBAAECggEATnDEkUfWbjP9MYAtD/MMZm03MJUtm2JnydFRDEBm8mb0
9X1NMq62d98I/v14de6UBZIpF1VTDB9WTBW7c8YzPkkwXg7eOjB92TLkJzK16lil
kGWSP4BALWQGeMe4tTRUJ3M13thYxv/Fb1fzyE+PX/jpwDtVRmZAMAT/zzfi0AmB
ZoVcN0ipr4cd8xq6SEk3rLz4UqstX4qZ58U0Yqhi9q5YVzMrwV4GXHf80zaq1DYn
m5Unddq1Igs0HDjTbHtY0jyQW3IK1fNr0DLvmw52AeQD/xYWB83EWDPITrMFuNa4
GYEGlnawalLyteh1/mgJZ/F/gqNcWNefkN5FDqtXIQKBgQDR/nriVyk12QXzDR91
7CAJRrEMl3QlvRgfqdVYveiC7osvM8MHQzE+yEfZYTMvOhBs4J5QH0bIfcLiGgeK
nyXnDqJx+gJr4dYOX2Ma/DT1wghItPyxCFsbGvL0c0LDewaL3NcQyPIFip6O2f2Z
jjlrHnXThbtcByH2y9i/oVWJ0QKBgQDMW0bpcXNBIfllZDQOUl4PfPnxTS67qZjC
3QpqHqdKqNs7nNutJNLuOaJig14stjTm5EdliwZPvOPAnbjEOdP5vG1GxwD3GKRt
G6MdPT2TdKCHaPOv/gE6tU2nt2wk7OsmTE20aDd8Czwc3l8ok8/KAM7YXOjbsg8y
StacIrDzjwKBgGD89Uqm0RzSwKGyVO6FAYLnSIy1QurPPF4bdbNH+yTGAjjp4lDv
YsZJgI3RC+/rFH0E/XmqCGo/U4xUU1leCgZ+xt53hzjGlLQMsFxdUiST2nmiRfeK
EXIib0YiGhrpLmvYsuhItyeCD5uQ6UVL4A8ugWMRqChoDvLK0bAoVraRAoGBAKig
EO2EHfR60k4l1waSVOc22w+P/qX/lfwFZRiX3rcuimiwUFyY7CyDBkl/2O/QEesM
JxXVGAon9U71VockqJOENi+W+mbqwJL/oSL5a5wHOodBxQNN9zm4bTGdmvEFRiw9
/kiFTnNe2eWAYMkc3vLyo7vJPqQ6U2vOcTQ5NAZlAoGBAMBeXrXAO/+cAUt41oVE
Qoul7TG+QFTN4A650QVbHQZsSDsinLzgPQ8TLLFtb180OptXSvIR+OGfc0E9JcJe
xDWp3FkNixBAikmMzlBqq39+numIilltPwT4RLib4OP6x8gZZHUB/+3HIYmnxk/G
b9ZuffSh1GPmnXSyKb7rtg6M
-----END PRIVATE KEY-----"#;

struct StaticJwksSource {
    body: Vec<u8>,
    calls: Arc<AtomicUsize>,
    error: Option<CodexIdentityVerificationError>,
}

#[async_trait]
impl CodexJwksSource for StaticJwksSource {
    async fn fetch(&self) -> Result<Vec<u8>, CodexIdentityVerificationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = self.error {
            return Err(error);
        }
        Ok(self.body.clone())
    }
}

fn verifier() -> (CodexJwtIdentityVerifier, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let private = RsaPrivateKey::from_pkcs8_pem(PRIVATE_KEY).expect("parse test RSA key");
    let body = serde_json::to_vec(&json!({
        "keys": [{
            "kty": "RSA",
            "use": "sig",
            "alg": "RS256",
            "kid": KEY_ID,
            "n": URL_SAFE_NO_PAD.encode(private.n().to_bytes_be()),
            "e": URL_SAFE_NO_PAD.encode(private.e().to_bytes_be())
        }]
    }))
    .expect("serialize test JWKS");
    let verifier = CodexJwtIdentityVerifier::new(Box::new(StaticJwksSource {
        body,
        calls: Arc::clone(&calls),
        error: None,
    }));
    (verifier, calls)
}

fn claims(exp: u64) -> Value {
    let now = get_current_timestamp();
    json!({
        "iss": OFFICIAL_OPENAI_ISSUER,
        "aud": [OFFICIAL_OPENAI_API_AUDIENCE],
        "iat": now.saturating_sub(1),
        "nbf": now.saturating_sub(1),
        "exp": exp,
        "sub": "subject-signed",
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "account-signed",
            "chatgpt_user_id": "user-signed",
            "user_id": "user-signed",
            "chatgpt_plan_type": "plus"
        },
        "https://api.openai.com/profile": {
            "email": "signed@example.invalid"
        }
    })
}

fn token(claims: &Value) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(KEY_ID.to_owned());
    header.typ = Some("JWT".to_owned());
    let private = RsaPrivateKey::from_pkcs8_pem(PRIVATE_KEY).expect("parse signing key");
    let private_der = private.to_pkcs1_der().expect("encode signing key");
    encode(
        &header,
        claims,
        &EncodingKey::from_rsa_der(private_der.as_bytes()),
    )
    .expect("sign test access token")
}

fn secret(claims: &Value, refresh_token: Option<&str>) -> CodexOAuthSecret {
    CodexOAuthSecret {
        access_token: SecretString::new(token(claims).into()),
        refresh_token: refresh_token.map(|value| SecretString::new(value.to_owned().into())),
        id_token: None,
    }
}

fn id_token_claims(nonce: &str, subject: &str) -> Value {
    let now = get_current_timestamp();
    json!({
        "iss": OFFICIAL_OPENAI_ISSUER,
        "aud": [OFFICIAL_CODEX_OAUTH_CLIENT_ID],
        "iat": now.saturating_sub(1),
        "nbf": now.saturating_sub(1),
        "exp": now + 3_600,
        "sub": subject,
        "nonce": nonce
    })
}

#[tokio::test]
async fn verifier_should_derive_identity_only_from_valid_signed_claims() {
    let (verifier, calls) = verifier();
    let profile = verifier
        .verify(&secret(
            &claims(get_current_timestamp() + 3_600),
            Some("refresh-token"),
        ))
        .await
        .expect("verify signed Codex token");

    assert_eq!(profile.chatgpt_account_id, "account-signed");
    assert_eq!(profile.chatgpt_user_id.as_deref(), Some("user-signed"));
    assert_eq!(profile.email.as_deref(), Some("signed@example.invalid"));
    assert!(profile.next_refresh_at.is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn verifier_should_reject_expired_wrong_issuer_audience_or_unbound_tokens() {
    let (verifier, _) = verifier();
    let now = get_current_timestamp();
    let mut cases = vec![
        claims(now),
        claims(now + 3_600),
        claims(now + 3_600),
        claims(now + 3_600),
    ];
    cases[1]["iss"] = json!("https://attacker.invalid");
    cases[2]["aud"] = json!(["https://attacker.invalid"]);
    cases[3]["https://api.openai.com/auth"]
        .as_object_mut()
        .expect("auth claims")
        .remove("chatgpt_account_id");

    for claims in cases {
        let error = verifier
            .verify(&secret(&claims, None))
            .await
            .expect_err("invalid identity token must fail");
        assert_eq!(error, CodexIdentityVerificationError::Rejected);
    }
}

#[tokio::test]
async fn verifier_should_reject_user_rebinding_and_invalid_refresh_shape() {
    let (verifier, _) = verifier();
    let mut rebound = claims(get_current_timestamp() + 3_600);
    rebound["https://api.openai.com/auth"]["user_id"] = json!("other-user");
    let rebound_error = verifier
        .verify(&secret(&rebound, None))
        .await
        .expect_err("signed aliases must not disagree");
    let access = token(&claims(get_current_timestamp() + 3_600));
    let reused = CodexOAuthSecret {
        access_token: SecretString::new(access.clone().into()),
        refresh_token: Some(SecretString::new(access.into())),
        id_token: None,
    };
    let refresh_error = verifier
        .verify(&reused)
        .await
        .expect_err("refresh token must have independent shape");

    assert_eq!(rebound_error, CodexIdentityVerificationError::Rejected);
    assert_eq!(refresh_error, CodexIdentityVerificationError::Rejected);
}

#[tokio::test]
async fn verifier_should_cache_jwks_and_fail_closed_when_source_is_unavailable() {
    let (verifier, calls) = verifier();
    let valid = secret(&claims(get_current_timestamp() + 3_600), None);
    verifier.verify(&valid).await.expect("first verification");
    verifier.verify(&valid).await.expect("cached verification");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let unavailable = CodexJwtIdentityVerifier::new(Box::new(StaticJwksSource {
        body: Vec::new(),
        calls: Arc::new(AtomicUsize::new(0)),
        error: Some(CodexIdentityVerificationError::Unavailable),
    }));
    let error = unavailable
        .verify(&valid)
        .await
        .expect_err("unavailable JWKS must fail closed");
    assert_eq!(error, CodexIdentityVerificationError::Unavailable);
}

#[tokio::test]
async fn authorization_verifier_should_bind_signed_id_token_subject_audience_and_nonce() {
    let (verifier, _) = verifier();
    let access = secret(
        &claims(get_current_timestamp() + 3_600),
        Some("refresh-token"),
    );
    let nonce = SecretString::from("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
    let valid_id = SecretString::from(token(&id_token_claims(
        nonce.expose_secret(),
        "subject-signed",
    )));
    verifier
        .verify_authorization(&access, &valid_id, &nonce)
        .await
        .expect("verify authorization token set");

    let wrong_nonce = verifier
        .verify_authorization(
            &access,
            &valid_id,
            &SecretString::from("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"),
        )
        .await
        .expect_err("nonce mismatch must fail");
    let wrong_subject = verifier
        .verify_authorization(
            &access,
            &SecretString::from(token(&id_token_claims(
                nonce.expose_secret(),
                "other-subject",
            ))),
            &nonce,
        )
        .await
        .expect_err("subject mismatch must fail");
    let mut wrong_audience = id_token_claims(nonce.expose_secret(), "subject-signed");
    wrong_audience["aud"] = json!(["attacker-client"]);
    let wrong_audience = verifier
        .verify_authorization(&access, &SecretString::from(token(&wrong_audience)), &nonce)
        .await
        .expect_err("ID token audience mismatch must fail");

    assert_eq!(wrong_nonce, CodexIdentityVerificationError::Rejected);
    assert_eq!(wrong_subject, CodexIdentityVerificationError::Rejected);
    assert_eq!(wrong_audience, CodexIdentityVerificationError::Rejected);
}

#[derive(Deserialize)]
struct RealAccountExport {
    accounts: Vec<RealAccountWire>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RealAccountWire {
    token: String,
    refresh_token: Option<String>,
}

#[tokio::test]
async fn supplied_real_access_only_account_should_pass_official_signature_verification() {
    let Ok(path) = std::env::var("CODEX_REAL_ACCOUNT_FILE") else {
        return;
    };
    let bytes = std::fs::read(path).expect("read isolated real account fixture");
    let mut document: RealAccountExport =
        serde_json::from_slice(&bytes).expect("parse real account fixture shape");
    let account = document.accounts.pop().expect("one real account fixture");
    assert!(account.refresh_token.is_none());
    let verifier = CodexJwtIdentityVerifier::new(Box::new(
        ReqwestOpenAiJwksSource::new().expect("build official JWKS source"),
    ));
    let profile = verifier
        .verify(&CodexOAuthSecret {
            access_token: SecretString::new(account.token.into()),
            refresh_token: account
                .refresh_token
                .map(|token| SecretString::new(token.into())),
            id_token: None,
        })
        .await
        .expect("verify official access-only Codex account");

    assert!(!profile.chatgpt_account_id.is_empty());
    assert!(profile.access_token_expires_at.is_some());
    assert!(profile.next_refresh_at.is_none());
}
