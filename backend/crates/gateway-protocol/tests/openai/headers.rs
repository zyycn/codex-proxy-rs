//! 验证传输头部分类，以及响应中的连接级和身份字段过滤

use gateway_protocol::openai::{
    is_transport_managed_request_header, response_header_is_forwardable,
};

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
