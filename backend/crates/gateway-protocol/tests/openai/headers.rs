use gateway_protocol::openai::is_transport_managed_request_header;

#[test]
fn transport_headers_should_include_proxy_namespaces_and_compression() {
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
        "x-request-id",
        "accept-encoding",
        "content-encoding",
        "content-length",
        "host",
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
