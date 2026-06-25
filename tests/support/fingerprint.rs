use codex_proxy_rs::upstream::fingerprint::Fingerprint;

pub fn test_fingerprint() -> Fingerprint {
    Fingerprint::default_codex_desktop()
}

pub fn test_fingerprint_with_updated_at(updated_at: Option<&str>) -> Fingerprint {
    let mut fingerprint = test_fingerprint();
    fingerprint.updated_at = updated_at.map(ToString::to_string);
    fingerprint
}
