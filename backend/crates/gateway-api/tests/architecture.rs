use std::{fs, path::Path};

#[test]
fn manifest_should_depend_only_on_core_protocol_and_http_adapter_layers() {
    let manifest = include_str!("../Cargo.toml");
    let forbidden = [
        "codex-proxy-rs",
        "gateway-store",
        "provider-",
        "redis",
        "reqwest",
        "sqlx",
    ];

    assert!(
        forbidden
            .iter()
            .all(|dependency| !manifest.contains(dependency)),
        "gateway-api manifest contains an infrastructure/provider dependency"
    );
}

#[test]
fn workspace_should_include_gateway_api() {
    let workspace = include_str!("../../../Cargo.toml");

    assert!(workspace.contains("\"crates/gateway-api\""));
}

#[test]
fn source_tree_should_match_frozen_machine_manifest() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut actual = rust_files(&root.join("src"));
    actual.sort();
    let mut expected = vec![
        "src/admin/accounts.rs",
        "src/admin/auth.rs",
        "src/admin/client_keys.rs",
        "src/admin/mod.rs",
        "src/admin/observability.rs",
        "src/admin/settings.rs",
        "src/admin/system.rs",
        "src/admin/wire.rs",
        "src/health.rs",
        "src/lib.rs",
        "src/openai/auth.rs",
        "src/openai/error.rs",
        "src/openai/mod.rs",
        "src/openai/models.rs",
        "src/openai/responses/error.rs",
        "src/openai/responses/http.rs",
        "src/openai/responses/mod.rs",
        "src/openai/responses/request.rs",
        "src/openai/responses/response.rs",
        "src/openai/responses/websocket.rs",
        "src/openai/router.rs",
        "src/openai/service.rs",
    ];
    expected.sort_unstable();

    assert_eq!(actual, expected);
}

#[test]
fn test_tree_should_match_frozen_rust_mirror() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut actual = rust_files(&root.join("tests"));
    actual.sort();
    let mut expected = vec![
        "tests/admin/accounts.rs",
        "tests/admin/auth.rs",
        "tests/admin/client_keys.rs",
        "tests/admin/mod.rs",
        "tests/admin/observability.rs",
        "tests/admin/settings.rs",
        "tests/admin/system.rs",
        "tests/admin/wire.rs",
        "tests/architecture.rs",
        "tests/health.rs",
        "tests/main.rs",
        "tests/openai/auth.rs",
        "tests/openai/error.rs",
        "tests/openai/mod.rs",
        "tests/openai/models.rs",
        "tests/openai/responses/http.rs",
        "tests/openai/responses/mod.rs",
        "tests/openai/responses/websocket.rs",
        "tests/openai/router.rs",
    ];
    expected.sort_unstable();

    assert_eq!(actual, expected);
}

#[test]
fn frozen_openai_fixture_set_should_be_complete() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/openai/responses/fixtures");
    let files = all_files(&root);

    assert_eq!(files.len(), 69);
}

#[test]
fn admin_routes_should_use_only_get_post_and_static_paths() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/admin");
    let mut combined = String::new();
    for path in all_files(&root) {
        if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            combined.push_str(&fs::read_to_string(path).expect("read admin source"));
        }
    }

    assert!(
        !combined.contains("routing::put")
            && !combined.contains("routing::patch")
            && !combined.contains("routing::delete")
            && !combined.contains("/:id")
            && !combined.contains("/{id}")
    );
}

fn rust_files(root: &Path) -> Vec<String> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    all_files(root)
        .into_iter()
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("rs"))
        .map(|path| {
            path.strip_prefix(manifest)
                .expect("path below manifest")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

fn all_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).expect("read architecture directory") {
            let path = entry.expect("read architecture entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}
