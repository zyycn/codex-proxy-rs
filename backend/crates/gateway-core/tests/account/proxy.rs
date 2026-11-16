//! 验证代理端点的协议限制、凭据脱敏与非法配置拒绝

use gateway_core::account::OutboundProxy;

#[test]
fn proxy_endpoints_support_explicit_schemes_and_redact_credentials() {
    for scheme in ["http", "https", "socks5", "socks5h"] {
        let proxy =
            OutboundProxy::parse(&format!("{scheme}://user:p%40ss%3Aword@[::1]:1080")).unwrap();
        assert!(proxy.expose_url().contains("p%40ss%3Aword"));
        assert!(!proxy.endpoint().contains("user"));
        assert!(!proxy.endpoint().contains("p%40ss"));
        assert!(!format!("{proxy:?}").contains("user"));
        assert!(proxy.endpoint().contains("[::1]:1080"));
    }
}

#[test]
fn dial_endpoints_match_host_and_port_without_credentials_or_capacity() {
    for (source, same, different) in [
        (
            "http://user:password@Proxy.Example",
            "socks5://other:secret@proxy.example:80",
            "http://proxy.example:81",
        ),
        (
            "https://user:password@[::1]",
            "http://other:secret@[::1]:443",
            "https://[::2]",
        ),
    ] {
        let endpoint = OutboundProxy::parse(source).unwrap().dial_endpoint();
        let same = OutboundProxy::parse(same)
            .unwrap()
            .with_max_websocket_connections(3);
        assert_eq!(endpoint, same.dial_endpoint());
        assert!(same.matches_endpoint(&endpoint));
        assert!(
            !OutboundProxy::parse(different)
                .unwrap()
                .matches_endpoint(&endpoint)
        );
        assert_eq!(format!("{endpoint:?}"), "ProxyEndpoint(<redacted>)");
    }
}

#[test]
fn invalid_proxy_never_silently_becomes_direct() {
    for url in [
        "",
        "localhost:1080",
        "file:///etc/passwd",
        "ftp://host:21",
        "http://host:0",
        "socks5://host",
        "http://host:80/path",
        "http://host:80?secret=1",
        "http://host:80#fragment",
        "http://user:secret@host:80\n",
    ] {
        let error = OutboundProxy::parse(url).unwrap_err();
        assert!(!error.to_string().contains("secret"));
    }
}
