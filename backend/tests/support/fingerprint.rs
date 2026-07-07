use codex_proxy_rs::{
    config::schema::FingerprintConfig,
    upstream::fingerprint::{Fingerprint, RuntimeFingerprint},
};

pub(crate) fn test_fingerprint() -> Fingerprint {
    Fingerprint::from_config(&FingerprintConfig::default())
}

pub(crate) fn test_fingerprint_with_updated_at(updated_at: Option<&str>) -> Fingerprint {
    let mut fingerprint = test_fingerprint();
    fingerprint.updated_at = updated_at.map(ToString::to_string);
    fingerprint
}

pub(crate) fn runtime_test_fingerprint() -> RuntimeFingerprint {
    RuntimeFingerprint::new(test_fingerprint())
}

pub(crate) fn runtime_fingerprint(fingerprint: Fingerprint) -> RuntimeFingerprint {
    RuntimeFingerprint::new(fingerprint)
}
