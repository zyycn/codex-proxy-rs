use provider_openai::transport::profile::cli_release::parse_cli_release;
use serde_json::json;

#[test]
fn cli_latest_requires_matching_stable_platform_dependencies() {
    let mut manifest =
        json!({"name":"@openai/codex", "version":"0.155.0", "optionalDependencies": {}});
    for target in [
        "darwin-arm64",
        "darwin-x64",
        "linux-arm64",
        "linux-x64",
        "win32-arm64",
        "win32-x64",
    ] {
        manifest["optionalDependencies"][format!("@openai/codex-{target}")] =
            json!(format!("npm:@openai/codex@0.155.0-{target}"));
    }
    assert_eq!(
        parse_cli_release(&serde_json::to_vec(&manifest).unwrap()).unwrap(),
        "0.155.0"
    );
    let mut missing = manifest.clone();
    missing["optionalDependencies"]
        .as_object_mut()
        .unwrap()
        .remove("@openai/codex-linux-arm64");
    assert!(parse_cli_release(&serde_json::to_vec(&missing).unwrap()).is_err());
    for version in ["0.155.0-alpha.1", "0.155.0+custom", "invalid"] {
        manifest["version"] = json!(version);
        assert!(parse_cli_release(&serde_json::to_vec(&manifest).unwrap()).is_err());
    }
}
