//! 验证传输头部分类，以及响应中的连接级和身份字段过滤

use gateway_protocol::openai::{
    is_transport_managed_request_header, parse_retry_after_seconds, response_header_is_forwardable,
};

#[test]
fn retry_after_should_accept_nonnegative_seconds_and_http_dates() {
    for (header, expected) in [
        (" 30 ", Some(30)),
        ("0", Some(0)),
        ("Sun, 06 Nov 1994 08:49:37 GMT", Some(0)),
        ("Sunday, 06-Nov-94 08:49:37 GMT", Some(0)),
        ("Sun Nov  6 08:49:37 1994", Some(0)),
        ("+30", None),
        ("-1", None),
        ("1.5", None),
        ("18446744073709551616", None),
        ("", None),
        ("later", None),
    ] {
        assert_eq!(parse_retry_after_seconds(header), expected, "{header}");
    }
}

#[test]
fn retry_after_http_date_should_not_round_down_the_remaining_delay() {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let retry_at = UNIX_EPOCH + Duration::from_secs(now.as_secs() + 60);
    let header = httpdate::fmt_http_date(retry_at);
    let seconds = parse_retry_after_seconds(&header).expect("HTTP date");
    let remaining = retry_at.duration_since(SystemTime::now()).unwrap();
    assert!(Duration::from_secs(seconds) >= remaining);
    assert!(seconds <= 60);
}

#[test]
fn transport_headers_should_include_hop_fields_and_compression() {
    for name in [
        "accept-encoding",
        "content-encoding",
        "content-length",
        "host",
        "x-request-id",
        "connection",
        "keep-alive",
        "proxy-connection",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "sec-websocket-key",
        "sec-websocket-extensions",
    ] {
        assert!(is_transport_managed_request_header(name), "missing {name}");
    }
}

#[test]
fn transport_headers_should_leave_business_extensions_to_the_protocol_owner() {
    for name in [
        "cf-visitor",
        "cf-connecting-ip",
        "cf-connecting-ipv6",
        "cf-pseudo-ipv4",
        "cf-ray",
        "cf-ipcountry",
        "cf-warp-tag-id",
        "cf-worker",
        "cf-ew-via",
        "cf-future-proxy-field",
        "cdn-loop",
        "via",
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "x-forwarded-port",
        "x-forwarded-prefix",
        "x-forwarded-future-field",
        "x-real-ip",
        "true-client-ip",
        "x-openai-future-mode",
        "x-custom-extension",
        "x-client-request-id",
        "session-id",
        "thread-id",
        "x-codex-turn-state",
        "x-codex-beta-features",
        "openai-beta",
        "accept",
        "content-type",
        "x-cf-business-field",
    ] {
        assert!(
            !is_transport_managed_request_header(name),
            "unexpected {name}"
        );
    }
}

#[test]
fn response_headers_should_reject_hop_identity_and_dynamic_connection_fields() {
    let connection_options = vec!["x-hop".to_owned()];
    for name in [
        "connection",
        "x-hop",
        "content-length",
        "authorization",
        "set-cookie",
        "chatgpt-account-id",
        "x-openai-project",
        "sec-websocket-accept",
    ] {
        assert!(
            !response_header_is_forwardable(name, &connection_options),
            "unexpectedly exposed {name}"
        );
    }
    assert!(response_header_is_forwardable(
        "x-ratelimit-remaining-requests",
        &connection_options
    ));
}
