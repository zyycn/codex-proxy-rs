use std::sync::Arc;

use codex_proxy_rs::{
    bootstrap::{config::WireProfileConfig, services::wire_profile_from_config},
    upstream::openai::profile::CodexWireProfile,
};

use crate::support::storage::timestamp;

pub(crate) fn test_wire_profile_config() -> WireProfileConfig {
    WireProfileConfig {
        originator: "Codex Desktop".to_string(),
        codex_version: "0.144.2".to_string(),
        desktop_version: "26.707.72221".to_string(),
        desktop_build: "5307".to_string(),
        os_type: "Mac OS".to_string(),
        os_version: "15.7.1".to_string(),
        arch: "arm64".to_string(),
        terminal: "unknown".to_string(),
        verified_at: timestamp("2026-07-14T18:25:50Z"),
    }
}

pub(crate) fn test_wire_profile_value() -> CodexWireProfile {
    wire_profile_from_config(&test_wire_profile_config())
}

pub(crate) fn test_wire_profile() -> Arc<CodexWireProfile> {
    Arc::new(test_wire_profile_value())
}

pub(crate) fn wire_profile(profile: CodexWireProfile) -> Arc<CodexWireProfile> {
    Arc::new(profile)
}
