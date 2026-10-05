//! 检查 API 层依赖、源码与测试模块边界，以及管理路由约定

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
        "src/admin/account_groups.rs",
        "src/admin/accounts/credentials.rs",
        "src/admin/accounts/handlers.rs",
        "src/admin/accounts/import_tasks.rs",
        "src/admin/accounts/mod.rs",
        "src/admin/accounts/presenter.rs",
        "src/admin/accounts/wire.rs",
        "src/admin/auth.rs",
        "src/admin/backups.rs",
        "src/admin/client_keys.rs",
        "src/admin/extract.rs",
        "src/admin/mod.rs",
        "src/admin/observability/mod.rs",
        "src/admin/observability/presenter.rs",
        "src/admin/observability/query.rs",
        "src/admin/observability/routes.rs",
        "src/admin/observability/wire.rs",
        "src/admin/plugins/artifacts.rs",
        "src/admin/plugins/distribution.rs",
        "src/admin/plugins/instances.rs",
        "src/admin/plugins/management.rs",
        "src/admin/plugins/mod.rs",
        "src/admin/presenter.rs",
        "src/admin/proxies.rs",
        "src/admin/settings.rs",
        "src/admin/system.rs",
        "src/admin/wire.rs",
        "src/auth.rs",
        "src/health.rs",
        "src/key_usage/mod.rs",
        "src/key_usage/query.rs",
        "src/key_usage/presenter.rs",
        "src/lib.rs",
        "src/middleware/headers.rs",
        "src/middleware/mod.rs",
        "src/middleware/websocket.rs",
        "src/openai/middleware/http.rs",
        "src/openai/middleware/mod.rs",
        "src/openai/middleware/request.rs",
        "src/openai/middleware/session.rs",
        "src/openai/auth.rs",
        "src/openai/error.rs",
        "src/openai/images/http.rs",
        "src/openai/images/mod.rs",
        "src/openai/live/http.rs",
        "src/openai/live/mod.rs",
        "src/openai/live/websocket.rs",
        "src/openai/mod.rs",
        "src/openai/models.rs",
        "src/openai/endpoint.rs",
        "src/openai/responses/error.rs",
        "src/openai/responses/http.rs",
        "src/openai/responses/mod.rs",
        "src/openai/responses/request.rs",
        "src/openai/responses/response.rs",
        "src/openai/responses/validation.rs",
        "src/openai/responses/websocket/connection/mod.rs",
        "src/openai/responses/websocket/connection/middleware.rs",
        "src/openai/responses/websocket/forward.rs",
        "src/openai/responses/websocket/mod.rs",
        "src/openai/responses/websocket/protocol.rs",
        "src/openai/router.rs",
        "src/openai/search/http.rs",
        "src/openai/search/mod.rs",
        "src/openai/service.rs",
        "src/openai/usage.rs",
        "src/provider.rs",
        "src/session_cookie.rs",
        "src/time.rs",
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
        "tests/admin/account_groups.rs",
        "tests/admin/accounts/handlers.rs",
        "tests/admin/accounts/import_tasks.rs",
        "tests/admin/accounts/mod.rs",
        "tests/admin/accounts/presenter.rs",
        "tests/admin/auth.rs",
        "tests/admin/client_keys.rs",
        "tests/admin/errors.rs",
        "tests/admin/mod.rs",
        "tests/admin/observability/mod.rs",
        "tests/admin/observability/query.rs",
        "tests/admin/observability/response.rs",
        "tests/admin/plugins/management.rs",
        "tests/admin/plugins/mod.rs",
        "tests/admin/proxies.rs",
        "tests/admin/settings.rs",
        "tests/admin/system.rs",
        "tests/admin/wire.rs",
        "tests/architecture.rs",
        "tests/auth.rs",
        "tests/support.rs",
        "tests/health.rs",
        "tests/key_usage/mod.rs",
        "tests/key_usage/fixtures.rs",
        "tests/main.rs",
        "tests/middleware/mod.rs",
        "tests/middleware/websocket.rs",
        "tests/openai/middleware/mod.rs",
        "tests/openai/auth.rs",
        "tests/openai/error.rs",
        "tests/openai/images/mod.rs",
        "tests/openai/mod.rs",
        "tests/openai/models.rs",
        "tests/openai/endpoint.rs",
        "tests/openai/responses/http.rs",
        "tests/openai/responses/mod.rs",
        "tests/openai/responses/request.rs",
        "tests/openai/responses/websocket/connection/mod.rs",
        "tests/openai/responses/websocket/connection/middleware.rs",
        "tests/openai/responses/websocket/forward.rs",
        "tests/openai/responses/websocket/interrupt.rs",
        "tests/openai/responses/websocket/mod.rs",
        "tests/openai/responses/websocket/protocol.rs",
        "tests/openai/live/mod.rs",
        "tests/openai/live/websocket.rs",
        "tests/openai/router.rs",
        "tests/openai/search/mod.rs",
        "tests/openai/usage.rs",
        "tests/provider.rs",
    ];
    expected.sort_unstable();

    assert_eq!(actual, expected);
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
