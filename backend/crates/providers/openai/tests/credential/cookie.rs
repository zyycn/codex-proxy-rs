use url::Url;

use provider_openai::credential::{CodexCookiePolicy, CookiePolicyError};

fn policy() -> CodexCookiePolicy {
    CodexCookiePolicy::new(["session"], ["chatgpt.com"]).expect("valid policy")
}

#[test]
fn capture_should_reject_parent_public_suffix_outside_allowlist() {
    let error = policy()
        .validate_capture(
            &Url::parse("https://chatgpt.com/backend-api").expect("valid URL"),
            Some("com"),
            "session",
            "/",
        )
        .err()
        .expect("public suffix must be rejected");

    assert_eq!(error, CookiePolicyError::InvalidScope);
}

#[test]
fn replay_should_respect_host_only_cookie_scope() {
    let policy = policy();

    assert!(!policy.may_replay(
        &Url::parse("https://api.chatgpt.com/backend-api").expect("valid URL"),
        "chatgpt.com",
        "/",
        true,
        true,
    ));
}

#[test]
fn replay_should_respect_secure_cookie_attribute() {
    let policy = policy();

    assert!(!policy.may_replay(
        &Url::parse("http://chatgpt.com/backend-api").expect("valid URL"),
        "chatgpt.com",
        "/",
        false,
        true,
    ));
}
