//! docs/architecture.md 依赖 DAG 与生产源码纪律的 workspace 级机器校验。

use std::{
    fs,
    path::{Path, PathBuf},
};

/// workspace 成员冻结清单;新增 crate 必须同步扩展本文件的依赖规则。
const WORKSPACE_MEMBERS: &[&str] = &[
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

fn backend_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("backend workspace root")
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
