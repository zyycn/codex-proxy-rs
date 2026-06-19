use std::{collections::BTreeMap, fs, path::PathBuf};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn workspace_dependencies_should_follow_architecture_direction() {
    let manifests = workspace_dependency_map();

    assert_no_workspace_deps(&manifests, "core");
    assert_no_workspace_deps(&manifests, "platform");
    assert_no_workspace_deps(&manifests, "assets");
    assert_forbidden_deps(&manifests, "adapters", &["runtime", "server"]);
    assert_forbidden_deps(&manifests, "runtime", &["server"]);
    assert_forbidden_deps(&manifests, "xtask", &["assets"]);
}

fn workspace_dependency_map() -> BTreeMap<String, Vec<String>> {
    let root = repository_root();
    [
        "core", "adapters", "runtime", "server", "platform", "assets", "xtask",
    ]
    .into_iter()
    .map(|crate_name| {
        let manifest_path = root.join("crates").join(crate_name).join("Cargo.toml");
        let manifest = fs::read_to_string(&manifest_path).unwrap_or_else(|_| {
            panic!("expected readable manifest at {}", manifest_path.display())
        });
        (crate_name.to_string(), workspace_deps(&manifest))
    })
    .collect()
}

fn workspace_deps(manifest: &str) -> Vec<String> {
    let mut deps = Vec::new();
    let mut in_dependencies = false;

    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_dependencies = trimmed == "[dependencies]";
            continue;
        }
        if !in_dependencies {
            continue;
        }
        for crate_name in [
            "codex-proxy-core",
            "codex-proxy-adapters",
            "codex-proxy-runtime",
            "codex-proxy-server",
            "codex-proxy-platform",
            "codex-proxy-assets",
        ] {
            if trimmed.starts_with(crate_name) {
                deps.push(crate_name.strip_prefix("codex-proxy-").unwrap().to_string());
            }
        }
    }

    deps
}

fn assert_no_workspace_deps(manifests: &BTreeMap<String, Vec<String>>, crate_name: &str) {
    let deps = manifests
        .get(crate_name)
        .unwrap_or_else(|| panic!("expected manifest entry for {crate_name}"));
    assert!(
        deps.is_empty(),
        "{crate_name} must not depend on workspace crates, found {deps:?}"
    );
}

fn assert_forbidden_deps(
    manifests: &BTreeMap<String, Vec<String>>,
    crate_name: &str,
    forbidden: &[&str],
) {
    let deps = manifests
        .get(crate_name)
        .unwrap_or_else(|| panic!("expected manifest entry for {crate_name}"));
    let violations = deps
        .iter()
        .filter(|dep| forbidden.contains(&dep.as_str()))
        .cloned()
        .collect::<Vec<_>>();

    assert!(
        violations.is_empty(),
        "{crate_name} has forbidden workspace dependencies: {violations:?}"
    );
}
