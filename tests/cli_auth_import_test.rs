use std::fs;

use codex_proxy_rs::auth::cli_import::{parse_cli_auth_json, read_cli_auth_from_home};

#[test]
fn parse_cli_auth_json_should_accept_codex_cli_tokens() {
    let auth = parse_cli_auth_json(
        r#"{
            "access_token": "cli-access-secret",
            "refresh_token": "cli-refresh-secret",
            "id_token": "cli-id-token",
            "expires_at": 1893456000
        }"#,
    )
    .unwrap();

    assert_eq!(auth.access_token(), "cli-access-secret");
    assert_eq!(auth.refresh_token(), Some("cli-refresh-secret"));
}

#[test]
fn parse_cli_auth_json_should_reject_missing_access_token() {
    let error = parse_cli_auth_json(r#"{"refresh_token":"cli-refresh-secret"}"#).unwrap_err();

    assert_eq!(
        error.to_string(),
        "CLI auth.json does not contain access_token"
    );
}

#[test]
fn read_cli_auth_from_home_should_load_auth_json_from_codex_home() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("auth.json"),
        r#"{"access_token":"file-access-secret","refresh_token":"file-refresh-secret"}"#,
    )
    .unwrap();

    let auth = read_cli_auth_from_home(dir.path()).unwrap();

    assert_eq!(auth.access_token(), "file-access-secret");
    assert_eq!(auth.refresh_token(), Some("file-refresh-secret"));
}
