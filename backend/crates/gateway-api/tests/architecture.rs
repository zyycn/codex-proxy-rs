//! 检查 API 层依赖与管理路由约定，模块布局和测试镜像由 workspace 架构检查覆盖

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
