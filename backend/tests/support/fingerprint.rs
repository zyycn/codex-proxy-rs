use codex_proxy_rs::{config::types::FingerprintConfig, upstream::fingerprint::Fingerprint};

pub(crate) fn test_fingerprint() -> Fingerprint {
    Fingerprint::from_config(&FingerprintConfig::default())
}

pub(crate) fn test_fingerprint_with_updated_at(updated_at: Option<&str>) -> Fingerprint {
    let mut fingerprint = test_fingerprint();
    fingerprint.updated_at = updated_at.map(ToString::to_string);
    fingerprint
}
