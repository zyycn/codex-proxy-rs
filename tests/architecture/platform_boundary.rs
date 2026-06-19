use std::{fs, path::PathBuf};

#[test]
fn platform_exports_foundation_modules() {
    let platform_lib = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("crates/platform/src/lib.rs"),
    )
    .expect("expected platform lib.rs to be readable");

    assert!(
        platform_lib.contains("pub mod config;")
            && platform_lib.contains("pub mod crypto;")
            && platform_lib.contains("pub mod identity;")
            && platform_lib.contains("pub mod json;")
            && platform_lib.contains("pub mod logging;")
            && platform_lib.contains("pub mod storage;"),
        "platform crate should expose foundation modules directly from its own crate"
    );
}
