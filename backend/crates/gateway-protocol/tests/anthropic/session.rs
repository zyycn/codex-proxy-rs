//! Claude Code 会话来源优先级、格式兼容与无效身份边界

use gateway_protocol::anthropic::claude_session_id;
use serde_json::json;

#[test]
fn explicit_claude_sessions_follow_header_then_metadata() {
    let session = "12345678-1234-1234-1234-123456789abc";
    for (header, user_id, expected) in [
        (
            Some(" header-session "),
            json!("{\"session_id\":\"body-session\"}"),
            Some("header-session"),
        ),
        (
            None,
            json!("{\"device_id\":\"device\",\"session_id\":\"body-session\"}"),
            Some("body-session"),
        ),
        (
            None,
            json!(format!("user_device_account__session_{session}")),
            Some(session),
        ),
        (
            Some(""),
            json!(format!("user_{}_session_{session}", "x".repeat(300))),
            Some(session),
        ),
        (None, json!("ordinary-user-id"), None),
        (None, json!("{\"device_id\":\"device\"}"), None),
        (None, json!("{invalid}"), None),
        (None, json!({"session_id":"not-a-string"}), None),
        (None, json!("user_device_session_"), None),
        (None, json!("user_device_session_not-a-uuid"), None),
    ] {
        let body =
            json!({"metadata":{"user_id":user_id},"messages":[{"role":"user","content":"hello"}]});
        assert_eq!(
            claude_session_id(body.as_object().unwrap(), header).as_deref(),
            expected
        );
    }
}

#[test]
fn invalid_session_signals_do_not_bind_users_or_message_content() {
    for invalid in [String::new(), "x".repeat(257), "bad\nidentity".to_owned()] {
        let body = json!({"metadata":{"user_id":json!({"session_id":invalid}).to_string()}});
        assert_eq!(
            claude_session_id(body.as_object().unwrap(), Some(&invalid)),
            None
        );
        let fallback = json!({"metadata":{"user_id":"{\"session_id\":\"valid-fallback\"}"}});
        assert_eq!(
            claude_session_id(fallback.as_object().unwrap(), Some(&invalid)).as_deref(),
            Some("valid-fallback")
        );
    }
    assert_eq!(
        claude_session_id(
            json!({"messages":[{"role":"user","content":"same opening"}]})
                .as_object()
                .unwrap(),
            None
        ),
        None
    );
}
