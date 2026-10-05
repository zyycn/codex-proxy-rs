use gateway_core::live::{call_id_from_location, is_valid_call_id};

#[test]
fn call_ids_accept_only_upstream_identifier_characters() {
    assert!(is_valid_call_id("call_abcd-123"));
    assert!(is_valid_call_id(
        "RTCV_0123456789abcdefghijklmnopqrstuvwxyz"
    ));
    assert!(!is_valid_call_id(""));
    assert!(!is_valid_call_id("bad id"));
    assert!(!is_valid_call_id("slash/id"));
    assert!(!is_valid_call_id(&"a".repeat(129)));
}

#[test]
fn location_call_id_accepts_the_three_official_shapes() {
    assert_eq!(
        call_id_from_location("call_abcd-123"),
        Some("call_abcd-123".to_owned())
    );
    assert_eq!(
        call_id_from_location("https://api.openai.com/v1/live/call_1"),
        Some("call_1".to_owned())
    );
    assert_eq!(
        call_id_from_location("https://api.openai.com/v1/realtime/calls/call_2"),
        Some("call_2".to_owned())
    );
    assert_eq!(
        call_id_from_location("/backend-api/codex/realtime/calls/call_3?x=1"),
        Some("call_3".to_owned())
    );
    assert_eq!(
        call_id_from_location("/v1/live?call_id=call_4"),
        Some("call_4".to_owned())
    );
}

#[test]
fn location_without_a_parseable_call_id_is_rejected() {
    assert_eq!(call_id_from_location("/v1/live"), None);
    assert_eq!(call_id_from_location("/v1/live?call_id=bad id"), None);
    assert_eq!(call_id_from_location("/v1/live?other=call_1"), None);
    assert_eq!(call_id_from_location("/v1/realtime/wrong/call_1"), None);
}
