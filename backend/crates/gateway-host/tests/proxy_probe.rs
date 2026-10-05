//! 验证代理探测的双栈地址、认证、证书与失败边界

use gateway_admin::ports::proxy::ProxyProbe;
use gateway_core::account::OutboundProxy;
use gateway_host::proxy_probe::HttpProxyProbe;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{any, header, path},
};

#[tokio::test]
async fn proxy_probe_supports_ipv4_and_ipv6_proxies_and_exit_addresses() {
    for (listen_address, exit_ip) in [
        ("127.0.0.1:0", "203.0.113.8"),
        ("127.0.0.1:0", "2001:db8::8"),
        ("[::1]:0", "203.0.113.8"),
        ("[::1]:0", "2001:db8::8"),
    ] {
        let listener = std::net::TcpListener::bind(listen_address).unwrap();
        let proxy_server = MockServer::builder().listener(listener).start().await;
        Mock::given(header("proxy-authorization", "Basic dXNlcjpwYXNzd29yZA=="))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ip": exit_ip})))
            .expect(1)
            .mount(&proxy_server)
            .await;
        let proxy =
            OutboundProxy::parse(&format!("http://user:password@{}", proxy_server.address()))
                .unwrap();
        let result = HttpProxyProbe::new("http://unresolvable.invalid/ip")
            .test(&proxy, false)
            .await;
        assert!(
            result.success,
            "{listen_address} -> {exit_ip}: {}",
            result.message
        );
        assert_eq!(result.exit_ip.unwrap().to_string(), exit_ip);
    }
}

#[tokio::test]
async fn proxy_probe_rejects_auth_errors_redirects_and_invalid_or_oversized_responses() {
    for response in [
        ResponseTemplate::new(407),
        ResponseTemplate::new(302).insert_header("Location", "http://127.0.0.1/"),
        ResponseTemplate::new(200).set_body_json(json!({"ip": "not-an-ip"})),
        ResponseTemplate::new(200).set_body_string("a".repeat(1025)),
    ] {
        let proxy_server = MockServer::start().await;
        Mock::given(any())
            .respond_with(response)
            .expect(1)
            .mount(&proxy_server)
            .await;
        let result = HttpProxyProbe::new("http://unresolvable.invalid/ip")
            .test(&OutboundProxy::parse(&proxy_server.uri()).unwrap(), false)
            .await;
        assert!(!result.success);
        assert!(result.exit_ip.is_none());
    }
}

#[tokio::test]
async fn unavailable_proxy_never_falls_back_to_direct_connection() {
    let target = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ip":"203.0.113.8"})))
        .expect(0)
        .mount(&target)
        .await;
    let unused = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = OutboundProxy::parse(&format!("http://{}", unused.local_addr().unwrap())).unwrap();
    drop(unused);
    let result = HttpProxyProbe::new(target.uri()).test(&proxy, false).await;
    assert!(!result.success);
}

#[tokio::test]
async fn invalid_certificate_configuration_should_not_fall_back_or_expose_details() {
    let target = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&target)
        .await;
    let result = HttpProxyProbe::new(target.uri())
        .with_client_builder(|_| Err("private-certificate-path"))
        .test(&OutboundProxy::parse(&target.uri()).unwrap(), false)
        .await;
    assert!(!result.success);
    assert!(!result.message.contains("private-certificate-path"));
}

#[tokio::test]
async fn dual_stack_proxy_probe_reports_both_addresses_when_available() {
    let proxy_server = MockServer::start().await;
    Mock::given(path("/v4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ip": "203.0.113.8"})))
        .expect(1)
        .mount(&proxy_server)
        .await;

    Mock::given(path("/v6"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ip": "2001:db8::8"})))
        .expect(1)
        .mount(&proxy_server)
        .await;

    let proxy = OutboundProxy::parse(&proxy_server.uri()).unwrap();
    let result = HttpProxyProbe::new_dual(
        format!("{}/v4", proxy_server.uri()),
        format!("{}/v6", proxy_server.uri()),
    )
    .test(&proxy, false)
    .await;

    assert!(result.success);
    assert_eq!(result.exit_ipv4.unwrap().to_string(), "203.0.113.8");
    assert_eq!(result.exit_ipv6.unwrap().to_string(), "2001:db8::8");
    assert!(result.message.contains("双栈可用"));
}

#[tokio::test]
async fn dual_stack_proxy_probe_reports_single_stack_when_only_one_succeeds() {
    let proxy_server = MockServer::start().await;
    Mock::given(path("/v4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ip": "203.0.113.8"})))
        .expect(1)
        .mount(&proxy_server)
        .await;

    Mock::given(path("/v6"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&proxy_server)
        .await;

    let proxy = OutboundProxy::parse(&proxy_server.uri()).unwrap();
    let result = HttpProxyProbe::new_dual(
        format!("{}/v4", proxy_server.uri()),
        format!("{}/v6", proxy_server.uri()),
    )
    .test(&proxy, false)
    .await;

    assert!(result.success);
    assert_eq!(result.exit_ipv4.unwrap().to_string(), "203.0.113.8");
    assert!(result.exit_ipv6.is_none());
    assert!(result.message.contains("仅 IPv4"));
}

fn location_response(ip: &str, timezone: &str) -> serde_json::Value {
    json!({"success": true, "ip": ip, "country_code": "JP", "region": "Tokyo", "city": "Tokyo", "timezone": {"id": timezone}})
}

#[tokio::test]
async fn location_lookup_uses_verified_exit_ip_without_requiring_proxy_access_to_geo_service() {
    use gateway_admin::model::proxies::ProxyLocationDetection;

    let proxy_server = MockServer::start().await;
    Mock::given(path("/ip"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ip": "203.0.113.8"})))
        .expect(1)
        .mount(&proxy_server)
        .await;
    let location_server = MockServer::start().await;
    Mock::given(path("/geo/203.0.113.8"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(location_response("203.0.113.8", "Asia/Tokyo")),
        )
        .expect(1)
        .mount(&location_server)
        .await;

    let result = HttpProxyProbe::new("http://unresolvable.invalid/ip")
        .with_location_endpoint(format!("{}/geo", location_server.uri()))
        .test(&OutboundProxy::parse(&proxy_server.uri()).unwrap(), true)
        .await;

    assert!(result.success);
    assert!(matches!(
        result.location,
        ProxyLocationDetection::Detected { .. }
    ));
}

#[tokio::test]
async fn location_lookup_uses_probed_ips_through_the_proxy_and_checks_both_timezones() {
    use gateway_admin::model::proxies::ProxyLocationDetection;
    for (v6_timezone, conflict) in [("Asia/Tokyo", false), ("America/New_York", true)] {
        let proxy_server = MockServer::start().await;
        for (endpoint, ip) in [("/v4", "203.0.113.8"), ("/v6", "2001:db8::8")] {
            Mock::given(path(endpoint))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ip": ip})))
                .expect(1)
                .mount(&proxy_server)
                .await;
        }
        for (ip, timezone) in [("203.0.113.8", "Asia/Tokyo"), ("2001:db8::8", v6_timezone)] {
            Mock::given(path(format!("/geo/{ip}")))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(location_response(ip, timezone)),
                )
                .expect(1)
                .mount(&proxy_server)
                .await;
        }
        let result = HttpProxyProbe::new_dual(
            "http://unresolvable.invalid/v4",
            "http://unresolvable.invalid/v6",
        )
        .with_location_endpoint("http://unresolvable.invalid/geo/")
        .test(&OutboundProxy::parse(&proxy_server.uri()).unwrap(), true)
        .await;
        assert!(result.success);
        if conflict {
            assert_eq!(result.location, ProxyLocationDetection::Conflict);
        } else {
            let ProxyLocationDetection::Detected { location } = result.location else {
                panic!("location missing")
            };
            assert_eq!(location.timezone.name(), "Asia/Tokyo");
            assert_eq!(location.city, "Tokyo");
        }
    }
}

#[tokio::test]
async fn geolocation_failure_never_turns_connectivity_into_failure_or_uses_unverified_data() {
    use gateway_admin::model::proxies::ProxyLocationDetection;
    let mut missing_city = location_response("203.0.113.8", "Asia/Tokyo");
    missing_city["city"] = json!("");
    for response in [
        ResponseTemplate::new(429),
        ResponseTemplate::new(302)
            .insert_header("Location", "http://unresolvable.invalid/redirect"),
        ResponseTemplate::new(200).set_body_json(json!({"success":false})),
        ResponseTemplate::new(200).set_body_json(location_response("203.0.113.9", "Asia/Tokyo")),
        ResponseTemplate::new(200).set_body_json(location_response("203.0.113.8", "Asia/Typo")),
        ResponseTemplate::new(200).set_body_json(missing_city),
        ResponseTemplate::new(200).set_body_string("x".repeat(8193)),
    ] {
        let proxy_server = MockServer::start().await;
        Mock::given(path("/ip"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ip": "203.0.113.8"})))
            .expect(1)
            .mount(&proxy_server)
            .await;
        Mock::given(path("/geo/203.0.113.8"))
            .respond_with(response)
            .expect(1)
            .mount(&proxy_server)
            .await;
        let result = HttpProxyProbe::new("http://unresolvable.invalid/ip")
            .with_location_endpoint("http://unresolvable.invalid/geo")
            .test(&OutboundProxy::parse(&proxy_server.uri()).unwrap(), true)
            .await;
        assert!(result.success);
        assert!(matches!(
            result.location,
            ProxyLocationDetection::Failed { .. }
        ));
    }
}

#[tokio::test]
async fn disabled_geolocation_never_calls_the_location_service() {
    let proxy_server = MockServer::start().await;
    Mock::given(path("/ip"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ip": "203.0.113.8"})))
        .expect(1)
        .mount(&proxy_server)
        .await;
    Mock::given(path("/geo/203.0.113.8"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&proxy_server)
        .await;
    let result = HttpProxyProbe::new("http://unresolvable.invalid/ip")
        .with_location_endpoint("http://unresolvable.invalid/geo")
        .test(&OutboundProxy::parse(&proxy_server.uri()).unwrap(), false)
        .await;
    assert!(result.success);
}
