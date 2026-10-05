//! 验证账号代理位置的规范化、字段约束与代理变更后的清理

use gateway_core::account::{InvalidRequestLocation, RequestLocation};
use serde_json::json;

fn location() -> RequestLocation {
    RequestLocation {
        country: "JP".to_owned(),
        region: "Tokyo".to_owned(),
        city: "Tokyo".to_owned(),
        timezone: chrono_tz::Asia::Tokyo,
    }
}

#[test]
fn changing_or_removing_proxy_drops_its_location() {
    use gateway_core::account::OutboundProxy;
    let direct = super::account("acct_location").with_request_location(Some(location()));
    assert!(direct.request_location().is_none());
    let proxied = direct
        .with_outbound_proxy(Some(OutboundProxy::parse("http://localhost:8080").unwrap()))
        .with_request_location(Some(location()));
    assert_eq!(proxied.request_location(), Some(&location()));
    assert!(
        proxied
            .clone()
            .with_outbound_proxy(None)
            .request_location()
            .is_none()
    );
    assert!(
        proxied
            .with_outbound_proxy(Some(OutboundProxy::parse("http://localhost:8081").unwrap()))
            .request_location()
            .is_none()
    );
}

#[test]
fn location_normalizes_names_and_preserves_timezone() {
    let value = RequestLocation {
        region: "  Tokyo  ".to_owned(),
        city: " 東京 ".to_owned(),
        ..location()
    }
    .normalized()
    .expect("valid location");
    assert_eq!(value.region, "Tokyo");
    assert_eq!(value.city, "東京");
    assert_eq!(value.timezone.name(), "Asia/Tokyo");
    let encoded = serde_json::to_value(&value).expect("serialize");
    assert_eq!(encoded["timezone"], "Asia/Tokyo");
    assert_eq!(
        serde_json::from_value::<RequestLocation>(encoded).unwrap(),
        value
    );
}

#[test]
fn location_rejects_invalid_country_codes() {
    for country in ["", "J", "JPN", "jp", "J1", "日", " JP"] {
        assert_eq!(
            RequestLocation {
                country: country.to_owned(),
                ..location()
            }
            .validate(),
            Err(InvalidRequestLocation::Country)
        );
    }
}

#[test]
fn location_rejects_empty_control_and_long_names() {
    for name in [
        String::new(),
        "  ".to_owned(),
        "Tokyo\n".to_owned(),
        "\tTokyo".to_owned(),
        "To\0kyo".to_owned(),
        "東".repeat(129),
    ] {
        assert_eq!(
            RequestLocation {
                region: name.clone(),
                ..location()
            }
            .normalized(),
            Err(InvalidRequestLocation::Region)
        );
        assert_eq!(
            RequestLocation {
                city: name,
                ..location()
            }
            .normalized(),
            Err(InvalidRequestLocation::City)
        );
    }
    assert!(
        RequestLocation {
            city: "東".repeat(128),
            ..location()
        }
        .validate()
        .is_ok()
    );
}

#[test]
fn location_requires_complete_object_and_valid_iana_timezone() {
    for timezone in ["Asia/Tokyo", "America/New_York", "Europe/London"] {
        assert!(
            serde_json::from_value::<RequestLocation>(json!({
                "country": "JP", "region": "Tokyo", "city": "Tokyo", "timezone": timezone
            }))
            .is_ok()
        );
    }
    for timezone in ["", "Not/A_Zone", "+09:00", "Asia/Typo"] {
        assert!(
            serde_json::from_value::<RequestLocation>(json!({
                "country": "JP", "region": "Tokyo", "city": "Tokyo", "timezone": timezone
            }))
            .is_err()
        );
    }
    assert!(serde_json::from_value::<RequestLocation>(json!({"country": "JP"})).is_err());
}
