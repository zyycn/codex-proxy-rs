//! docs/architecture.md 依赖 DAG 与生产源码纪律的 workspace 级机器校验。

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use syn::Item;

/// workspace 成员冻结清单;新增 crate 必须同步扩展本文件的依赖规则。
pub(super) const WORKSPACE_MEMBERS: &[&str] = &[
    "apps/gateway",
    "crates/gateway-admin",
    "crates/gateway-api",
    "crates/gateway-core",
    "crates/gateway-host",
    "crates/gateway-protocol",
    "crates/gateway-store",
    "crates/providers/openai",
    "crates/providers/xai",
];

#[test]
fn workspace_member_list_matches_the_frozen_dag_scope() {
    let manifest =
        fs::read_to_string(backend_root().join("Cargo.toml")).expect("read workspace manifest");
    for member in WORKSPACE_MEMBERS {
        assert!(
            manifest.contains(&format!("\"{member}\"")),
            "workspace must include {member}"
        );
    }
    let member_count = manifest
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("\"apps/") || line.starts_with("\"crates/"))
        .count();
    assert_eq!(
        member_count,
        WORKSPACE_MEMBERS.len(),
        "new workspace members must extend the dependency DAG rules"
    );
}

#[test]
fn gateway_core_depends_on_no_http_db_redis_or_provider_crate() {
    assert_no_dependency(
        "crates/gateway-core",
        &[
            "axum",
            "hyper",
            "reqwest",
            "sqlx",
            "redis",
            "gateway-host",
            "gateway-store",
            "provider-openai",
            "provider-xai",
        ],
    );
}

#[test]
fn gateway_protocol_has_no_workspace_dependencies() {
    for name in dependency_names("crates/gateway-protocol") {
        assert!(
            !name.starts_with("gateway-") && !name.starts_with("provider-"),
            "gateway-protocol must not depend on workspace crate `{name}`"
        );
    }
}

#[test]
fn provider_crates_do_not_depend_on_each_other() {
    assert_no_dependency("crates/providers/openai", &["provider-xai"]);
    assert_no_dependency("crates/providers/xai", &["provider-openai"]);
}

#[test]
fn gateway_admin_stays_free_of_infrastructure_dependencies() {
    assert_no_dependency(
        "crates/gateway-admin",
        &["axum", "sqlx", "redis", "reqwest"],
    );
}

/// workspace 包名到冻结成员路径的映射。
const PACKAGE_TO_MEMBER: &[(&str, &str)] = &[
    ("codex-proxy-rs", "apps/gateway"),
    ("gateway-admin", "crates/gateway-admin"),
    ("gateway-api", "crates/gateway-api"),
    ("gateway-core", "crates/gateway-core"),
    ("gateway-host", "crates/gateway-host"),
    ("gateway-protocol", "crates/gateway-protocol"),
    ("gateway-store", "crates/gateway-store"),
    ("provider-openai", "crates/providers/openai"),
    ("provider-xai", "crates/providers/xai"),
];

/// Adapter/provider 根门面暂时允许公开的模块；收窄公开面时必须同步缩小本表。
const ADAPTER_PUBLIC_MODULES: &[(&str, &[&str])] = &[
    ("crates/gateway-api", &["admin", "openai"]),
    (
        "crates/gateway-host",
        &[
            "client_distribution",
            "config",
            "serve",
            "system_update",
            "workers",
        ],
    ),
    ("crates/gateway-store", &["backup", "postgres", "redis"]),
    (
        "crates/providers/openai",
        &["config", "credential", "transport"],
    ),
    (
        "crates/providers/xai",
        &["config", "credential", "transport"],
    ),
];

/// 冻结的 workspace 内部运行时依赖边；新增/删除任何边都必须同步本表。
const ALLOWED_INTERNAL_EDGES: &[(&str, &str)] = &[
    ("codex-proxy-rs", "gateway-admin"),
    ("codex-proxy-rs", "gateway-api"),
    ("codex-proxy-rs", "gateway-core"),
    ("codex-proxy-rs", "gateway-host"),
    ("codex-proxy-rs", "gateway-store"),
    ("codex-proxy-rs", "provider-openai"),
    ("codex-proxy-rs", "provider-xai"),
    ("gateway-admin", "gateway-core"),
    ("gateway-api", "gateway-admin"),
    ("gateway-api", "gateway-core"),
    ("gateway-api", "gateway-protocol"),
    ("gateway-host", "gateway-admin"),
    ("gateway-host", "gateway-core"),
    ("gateway-store", "gateway-admin"),
    ("gateway-store", "gateway-core"),
    ("provider-openai", "gateway-admin"),
    ("provider-openai", "gateway-core"),
    ("provider-openai", "gateway-protocol"),
    ("provider-xai", "gateway-admin"),
    ("provider-xai", "gateway-core"),
    ("provider-xai", "gateway-protocol"),
];

#[test]
fn workspace_internal_dependency_edges_match_frozen_dag() {
    let metadata = cargo_metadata_json();
    let packages = metadata["packages"]
        .as_array()
        .expect("cargo metadata packages");
    let internal_names: std::collections::BTreeSet<&str> =
        PACKAGE_TO_MEMBER.iter().map(|(name, _)| *name).collect();

    let mut actual: Vec<(String, String)> = Vec::new();
    for package in packages {
        let name = package["name"].as_str().expect("package name");
        if !internal_names.contains(name) {
            continue;
        }
        for dependency in package["dependencies"].as_array().into_iter().flatten() {
            let dep_name = dependency["name"].as_str().expect("dependency name");
            if !internal_names.contains(dep_name) {
                continue;
            }
            let is_runtime_edge = dependency["dep_kinds"]
                .as_array()
                .map(|kinds| {
                    kinds
                        .iter()
                        .any(|kind| kind["kind"].as_str() == Some("normal"))
                })
                .unwrap_or(true);
            if is_runtime_edge {
                actual.push((name.to_owned(), dep_name.to_owned()));
            }
        }
    }

    let mut expected: Vec<(String, String)> = ALLOWED_INTERNAL_EDGES
        .iter()
        .map(|(from, to)| ((*from).to_owned(), (*to).to_owned()))
        .collect();
    actual.sort();
    expected.sort();

    assert_eq!(
        actual, expected,
        "workspace internal dependency edges diverged from the frozen DAG"
    );
}

#[test]
fn adapter_public_module_surfaces_match_allowlist() {
    for (member, allowed) in ADAPTER_PUBLIC_MODULES {
        let root = backend_root().join(member).join("src/lib.rs");
        let source = fs::read_to_string(&root).expect("read adapter crate root");
        let syntax = syn::parse_file(&source).expect("parse adapter crate root");
        let actual = syntax
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Mod(module) if matches!(module.vis, syn::Visibility::Public(_)) => {
                    Some(module.ident.to_string())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let expected = allowed
            .iter()
            .map(|module| (*module).to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            actual,
            expected,
            "{} public module surface diverged from its allowlist",
            root.display()
        );
    }
}

#[test]
fn production_sources_do_not_host_tests() {
    for member in WORKSPACE_MEMBERS {
        let src = backend_root().join(member).join("src");
        let sources = super::rust_files(&src);
        assert!(!sources.is_empty(), "{member} has no production sources");
        for relative in sources {
            let path = src.join(relative);
            let source = fs::read_to_string(&path).expect("read production source");
            assert!(
                !source.contains("#[cfg(test)]"),
                "{} hosts tests in production src",
                path.display()
            );
        }
    }
}

#[test]
fn workspace_modules_follow_conventional_file_layout() {
    for member in WORKSPACE_MEMBERS {
        let member_root = backend_root().join(member);
        assert_module_tree(&member_root.join("src"), &["lib.rs", "main.rs"]);

        let tests = member_root.join("tests");
        if tests.is_dir() {
            assert_module_tree(&tests, &["main.rs"]);
        }
    }
}

fn assert_module_tree(root: &Path, crate_roots: &[&str]) {
    let files = super::rust_files(root);
    for relative in &files {
        if crate_roots
            .iter()
            .any(|candidate| relative == Path::new(candidate))
        {
            continue;
        }

        let file_name = relative
            .file_name()
            .and_then(|value| value.to_str())
            .expect("Rust source file name");
        let parent = relative.parent().expect("Rust source parent");
        let module_name = if file_name == "mod.rs" {
            parent
                .file_name()
                .and_then(|value| value.to_str())
                .expect("mod.rs module name")
        } else {
            relative
                .file_stem()
                .and_then(|value| value.to_str())
                .expect("Rust source module name")
        };

        if file_name != "mod.rs" {
            assert_ne!(
                parent.file_name().and_then(|value| value.to_str()),
                Some(module_name),
                "{} repeats its parent module name",
                root.join(relative).display()
            );
            let child_directory = relative.with_extension("");
            assert!(
                !files
                    .iter()
                    .any(|candidate| candidate.starts_with(&child_directory)),
                "{} mixes a leaf module with a same-name module directory",
                root.join(relative).display()
            );
        }

        let declaration_parents = if parent.as_os_str().is_empty()
            || (file_name == "mod.rs"
                && parent
                    .parent()
                    .is_some_and(|ancestor| ancestor.as_os_str().is_empty()))
        {
            crate_roots
                .iter()
                .map(|candidate| root.join(candidate))
                .collect::<Vec<_>>()
        } else {
            let declaration_directory = if file_name == "mod.rs" {
                parent.parent().expect("nested mod.rs parent")
            } else {
                parent
            };
            vec![root.join(declaration_directory).join("mod.rs")]
        };
        let declaration_count = declaration_parents
            .iter()
            .map(|path| external_module_declaration_count(path, module_name))
            .sum::<usize>();
        assert_eq!(
            declaration_count,
            1,
            "{} must be declared exactly once by its parent module",
            root.join(relative).display()
        );
    }
}

fn external_module_declaration_count(path: &Path, module_name: &str) -> usize {
    if !path.is_file() {
        return 0;
    }
    let source = fs::read_to_string(path).expect("read parent module source");
    let syntax = syn::parse_file(&source).expect("parse parent module source");
    syntax
        .items
        .iter()
        .filter(|item| {
            matches!(
                item,
                Item::Mod(module)
                    if module.content.is_none() && module.ident == module_name
            )
        })
        .count()
}

pub(super) fn backend_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("backend workspace root")
}

fn cargo_metadata_json() -> serde_json::Value {
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(backend_root())
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse cargo metadata")
}

/// 提取成员 `[dependencies]` 段内声明的依赖名;段落以下一个 `[` 表头结束。
fn dependency_names(member: &str) -> Vec<String> {
    let manifest = fs::read_to_string(backend_root().join(member).join("Cargo.toml"))
        .expect("read member manifest");
    let mut in_dependencies = false;
    let mut names = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_dependencies = line == "[dependencies]";
            continue;
        }
        if !in_dependencies || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = line.split_once('=') {
            names.push(name.trim().to_owned());
        }
    }
    names
}

fn assert_no_dependency(member: &str, forbidden: &[&str]) {
    for name in dependency_names(member) {
        assert!(
            !forbidden.contains(&name.as_str()),
            "{member} must not depend on `{name}`"
        );
    }
}
