use codex_proxy_rs::cookies::jar::CookieJar;

#[test]
fn cookie_jar_captures_and_replays_account_scoped_cookies() {
    let mut jar = CookieJar::default();
    jar.capture_set_cookie(
        "acct_a",
        "cf_clearance=abc; Domain=chatgpt.com; Path=/; HttpOnly",
    );
    jar.capture_set_cookie(
        "acct_b",
        "cf_clearance=def; Domain=chatgpt.com; Path=/; HttpOnly",
    );

    assert_eq!(
        jar.cookie_header("acct_a", "chatgpt.com"),
        Some("cf_clearance=abc".to_string())
    );
    assert_eq!(
        jar.cookie_header("acct_b", "chatgpt.com"),
        Some("cf_clearance=def".to_string())
    );
}
