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
