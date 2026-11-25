//! 对应生产职责的行为测试

use super::*;

#[test]
fn restricted_details_preserve_causes_and_verbatim_upstream_body_separately() {
    let body = "{ \"error\": {\"code\":\"Vendor.Unknown\",\"message\":\"PRIVATE_BODY\"} }";
    let error = ProviderError::new(ProviderErrorKind::Unavailable, UpstreamSendState::Sent)
        .with_source(std::io::Error::other("PRIVATE_CAUSE"))
        .with_raw_upstream_error(RawUpstreamError::new(body));
    let details: serde_json::Value = serde_json::from_str(&error.error_details().unwrap()).unwrap();
    assert_eq!(details["upstream"], body);
    assert_eq!(details["causes"]["messages"], json!(["PRIVATE_CAUSE"]));
    assert_eq!(details["causes"]["truncated"], false);
    assert!(!format!("{error:?} {error}").contains("PRIVATE_"));
    assert!(
        ProviderError::new(ProviderErrorKind::Cancelled, UpstreamSendState::NotSent)
            .error_details()
            .is_none()
    );
}

#[test]
fn restricted_cause_snapshot_bounds_unicode_and_cycles_with_an_explicit_marker() {
    let error = ProviderError::new(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
        .with_source(std::io::Error::other("原始原因".repeat(20_000)));
    let details: serde_json::Value = serde_json::from_str(&error.error_details().unwrap()).unwrap();
    assert_eq!(details["causes"]["truncated"], true);
    assert!(details["causes"]["messages"][0].as_str().unwrap().len() <= 64 * 1024);

    #[derive(Debug)]
    struct CyclicError;
    impl std::fmt::Display for CyclicError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("cyclic source")
        }
    }
    impl std::error::Error for CyclicError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(self)
        }
    }
    let error = ProviderError::new(ProviderErrorKind::Protocol, UpstreamSendState::NotSent)
        .with_source(CyclicError);
    let details: serde_json::Value = serde_json::from_str(&error.error_details().unwrap()).unwrap();
    assert_eq!(details["causes"]["truncated"], true);
    assert_eq!(details["causes"]["messages"].as_array().unwrap().len(), 32);
}

#[test]
fn cleanup_failure_keeps_the_primary_cause_and_separate_bounded_details() {
    use gateway_core::error::{ErrorDetails, ErrorSource};
    let primary = ErrorSource::new(std::io::Error::other("PRIMARY_NATIVE_FAILURE"));
    let combined = primary.with_cleanup(std::io::Error::other("ROLLBACK_NATIVE_FAILURE"));
    assert_eq!(
        combined.source().unwrap().to_string(),
        "PRIMARY_NATIVE_FAILURE"
    );
    let details = ErrorDetails::capture(Some(&combined), None, false).unwrap();
    let value: serde_json::Value = serde_json::from_str(details.as_str()).unwrap();
    assert_eq!(
        value["causes"]["messages"],
        json!(["PRIMARY_NATIVE_FAILURE"])
    );
    assert_eq!(
        value["causes"]["cleanup"][0]["messages"],
        json!(["ROLLBACK_NATIVE_FAILURE"])
    );
    assert_eq!(value["causes"]["truncated"], false);
    assert!(!format!("{combined:?} {details:?}").contains("NATIVE_FAILURE"));

    let oversized = ErrorSource::new(std::io::Error::other("界".repeat(24_000)))
        .with_cleanup(std::io::Error::other("ROLLBACK_NATIVE_FAILURE"));
    let details = ErrorDetails::capture(Some(&oversized), None, false).unwrap();
    let value: serde_json::Value = serde_json::from_str(details.as_str()).unwrap();
    assert_eq!(value["causes"]["truncated"], true);
    assert_eq!(value["causes"]["cleanup"][0]["truncated"], true);
    assert!(details.as_str().len() < 66_000);
}
