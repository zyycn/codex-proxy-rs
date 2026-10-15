//! docs/architecture.md 依赖 DAG 与生产源码纪律的 workspace 级机器校验

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use syn::{Item, visit::Visit};

/// workspace 成员冻结清单;新增 crate 必须同步扩展本文件的依赖规则
pub(super) const WORKSPACE_MEMBERS: &[&str] = &[
    "apps/gateway",
    "apps/plugin-cli",
    "crates/gateway-admin",
    "crates/gateway-api",
    "crates/gateway-core",
    "crates/gateway-host",
    "crates/gateway-plugin/runtime",
    "crates/gateway-plugin/sdk",
    "crates/gateway-protocol",
    "crates/gateway-store",
    "crates/providers/openai",
    "crates/providers/xai",
];

#[test]
fn workspace_member_list_matches_the_frozen_dag_scope() {
    let metadata = cargo_metadata_json();
    let root = backend_root();
    let actual: BTreeSet<_> = metadata["packages"]
        .as_array()
        .expect("Cargo packages")
        .iter()
        .map(|package| {
            Path::new(package["manifest_path"].as_str().expect("package manifest"))
                .parent()
                .expect("package directory")
                .strip_prefix(&root)
                .expect("workspace member path")
                .to_path_buf()
        })
        .collect();
    assert_eq!(
        actual,
        WORKSPACE_MEMBERS.iter().map(PathBuf::from).collect(),
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
fn core_value_owners_do_not_depend_on_execution_or_routing() {
    let root = backend_root().join("crates/gateway-core/src");
    for relative in super::rust_files(&root) {
        let allowed: Option<&[&str]> = match relative.to_str().expect("source path") {
            "validation.rs" => Some(&[]),
            "identity.rs" => Some(&["validation"]),
            "upstream.rs" => Some(&["validation"]),
            "event.rs" => Some(&["metering", "operation", "upstream", "validation"]),
            "account/selection.rs" => Some(&["account", "concurrency", "identity", "validation"]),
            "concurrency.rs" => Some(&["error"]),
            "account/store.rs" => Some(&["account", "error", "identity", "validation"]),
            path if path.starts_with("policy/") => {
                Some(&["account", "identity", "policy", "validation"])
            }
            path if path.starts_with("settings/") => Some(&[
                "account",
                "concurrency",
                "identity",
                "metering",
                "policy",
                "settings",
            ]),
            path if path.starts_with("account/") => Some(&["account", "identity", "validation"]),
            _ => None,
        };
        let Some(allowed) = allowed else { continue };
        let source = fs::read_to_string(root.join(&relative)).expect("read core source");
        let syntax = syn::parse_file(&source).expect("parse core source");
        let mut references = CrateReferences::in_module(&relative);
        references.visit_file(&syntax);
        // 相对路径中的 self 指向当前 owner，不是跨 owner 依赖
        references.owners.remove(&references.module_path[0]);
        for dependency in references.owners {
            assert!(
                allowed.contains(&dependency.as_str()),
                "{} must not depend on crate::{dependency}",
                relative.display()
            );
        }
    }
}

#[test]
fn core_owner_paths_form_a_dag() {
    let root = backend_root().join("crates/gateway-core/src");
    let mut graph = BTreeMap::<String, BTreeSet<String>>::new();
    for relative in super::rust_files(&root) {
        if relative == Path::new("lib.rs") {
            continue;
        }
        let mut references = CrateReferences::in_module(&relative);
        let owner = references.module_path[0].clone();
        let source = fs::read_to_string(root.join(&relative)).expect("read core source");
        references.visit_file(&syn::parse_file(&source).expect("parse core source"));
        references.owners.remove(&owner);
        graph.entry(owner).or_default().extend(references.owners);
    }
    for owner in graph.keys() {
        let mut pending = graph[owner].iter().cloned().collect::<Vec<_>>();
        let mut visited = BTreeSet::new();
        while let Some(dependency) = pending.pop() {
            assert_ne!(
                &dependency, owner,
                "Core owner dependency cycle reaches {owner}"
            );
            if visited.insert(dependency.clone())
                && let Some(next) = graph.get(&dependency)
            {
                pending.extend(next.iter().cloned());
            }
        }
    }
}

#[test]
fn core_reference_scan_resolves_groups_reexports_and_relative_paths() {
    let syntax = syn::parse_file(
        r#"
        pub use crate::{engine::AttemptContext, settings::SettingsValues as Values};
        use super::super::runtime as publication;
        use self::compiled::Value;
        // crate::ignored::Comment
        const EXAMPLE: &str = "crate::ignored::Literal";
        "#,
    )
    .expect("parse reference fixture");
    let mut references = CrateReferences::in_module(Path::new("routing/snapshot.rs"));
    references.visit_file(&syntax);
    assert_eq!(
        references.owners,
        BTreeSet::from([
            "engine".to_owned(),
            "settings".to_owned(),
            "runtime".to_owned(),
            "routing".to_owned(),
        ])
    );
}

#[derive(Default)]
struct CrateReferences {
    owners: BTreeSet<String>,
    module_path: Vec<String>,
}

impl<'ast> Visit<'ast> for CrateReferences {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        self.record(
            &path
                .segments
                .iter()
                .map(|part| part.ident.to_string())
                .collect::<Vec<_>>(),
        );
        syn::visit::visit_path(self, path);
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.use_tree(&item.tree, &mut Vec::new());
    }
}

impl CrateReferences {
    fn in_module(relative: &Path) -> Self {
        let mut module = relative
            .with_extension("")
            .components()
            .map(|part| {
                part.as_os_str()
                    .to_str()
                    .expect("UTF-8 module path")
                    .to_owned()
            })
            .collect::<Vec<_>>();
        if module.last().is_some_and(|part| part == "mod") {
            module.pop();
        }
        Self {
            owners: BTreeSet::new(),
            module_path: module,
        }
    }

    fn record(&mut self, path: &[String]) {
        let Some(first) = path.first() else { return };
        let owner = if first == "crate" {
            path.get(1).cloned()
        } else if first == "self" || first == "super" {
            let mut module = self.module_path.clone();
            let mut rest = path;
            while rest
                .first()
                .is_some_and(|part| part == "super" || part == "self")
            {
                if rest[0] == "super" {
                    module.pop();
                }
                rest = &rest[1..];
            }
            module.first().or_else(|| rest.first()).cloned()
        } else {
            None
        };
        if let Some(owner) = owner {
            self.owners.insert(owner);
        }
    }

    fn use_tree(&mut self, tree: &syn::UseTree, prefix: &mut Vec<String>) {
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.use_tree(&path.tree, prefix);
                prefix.pop();
            }
            syn::UseTree::Name(name) => {
                prefix.push(name.ident.to_string());
                self.record(prefix);
                prefix.pop();
            }
            syn::UseTree::Rename(rename) => {
                prefix.push(rename.ident.to_string());
                self.record(prefix);
                prefix.pop();
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    self.use_tree(item, prefix);
                }
            }
            syn::UseTree::Glob(_) => self.record(prefix),
        }
    }
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
fn plugin_sdk_has_no_workspace_dependencies() {
    for name in dependency_names("crates/gateway-plugin/sdk") {
        assert!(!name.starts_with("gateway-") && !name.starts_with("provider-"));
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

/// workspace 包名到冻结成员路径的映射
const PACKAGE_TO_MEMBER: &[(&str, &str)] = &[
    ("codex-proxy-rs", "apps/gateway"),
    ("codex-proxy-plugin-cli", "apps/plugin-cli"),
    ("gateway-admin", "crates/gateway-admin"),
    ("gateway-api", "crates/gateway-api"),
    ("gateway-core", "crates/gateway-core"),
    ("gateway-host", "crates/gateway-host"),
    ("gateway-plugin-runtime", "crates/gateway-plugin/runtime"),
    ("gateway-plugin-sdk", "crates/gateway-plugin/sdk"),
    ("gateway-protocol", "crates/gateway-protocol"),
    ("gateway-store", "crates/gateway-store"),
    ("provider-openai", "crates/providers/openai"),
    ("provider-xai", "crates/providers/xai"),
];

/// SDK 与 Adapter/provider 根门面的稳定合同模块；任何增减都必须同步完成边界审计
const ADAPTER_PUBLIC_MODULES: &[(&str, &[&str])] = &[
    ("crates/gateway-plugin/sdk", &["call", "client"]),
    ("crates/gateway-plugin/runtime", &[]),
    ("crates/gateway-api", &["admin", "auth", "openai"]),
    (
        "crates/gateway-host",
        &[
            "client_distribution",
            "config",
            "outbound",
            "official_plugins",
            "pricing",
            "plugin_distribution",
            "process",
            "proxy_probe",
            "retention",
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
    ("crates/providers/xai", &["transport"]),
];

/// 不对应单一生产模块、而是校验 crate/workspace 整体契约的根级测试场景
const ROOT_TEST_SCENARIOS: &[(&str, &[&str])] = &[
    ("apps/gateway", &["architecture"]),
    ("crates/gateway-api", &["architecture"]),
];

/// 冻结的 workspace 内部运行时依赖边；新增/删除任何边都必须同步本表
const ALLOWED_INTERNAL_EDGES: &[(&str, &str)] = &[
    ("codex-proxy-plugin-cli", "gateway-plugin-sdk"),
    ("codex-proxy-rs", "gateway-admin"),
    ("codex-proxy-rs", "gateway-api"),
    ("codex-proxy-rs", "gateway-core"),
    ("codex-proxy-rs", "gateway-host"),
    ("codex-proxy-rs", "gateway-plugin-runtime"),
    ("codex-proxy-rs", "gateway-store"),
    ("codex-proxy-rs", "provider-openai"),
    ("codex-proxy-rs", "provider-xai"),
    ("gateway-admin", "gateway-core"),
    ("gateway-api", "gateway-admin"),
    ("gateway-api", "gateway-core"),
    ("gateway-api", "gateway-protocol"),
    ("gateway-host", "gateway-admin"),
    ("gateway-host", "gateway-core"),
    ("gateway-plugin-runtime", "gateway-host"),
    ("gateway-plugin-runtime", "gateway-core"),
    ("gateway-plugin-runtime", "gateway-admin"),
    ("gateway-plugin-runtime", "gateway-plugin-sdk"),
    ("gateway-plugin-runtime", "gateway-protocol"),
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
    let actual = workspace_runtime_dependency_edges(metadata);
    let mut expected: Vec<(String, String)> = ALLOWED_INTERNAL_EDGES
        .iter()
        .map(|(from, to)| ((*from).to_owned(), (*to).to_owned()))
        .collect();
    expected.sort();

    assert_eq!(
        actual, expected,
        "workspace internal dependency edges diverged from the frozen DAG"
    );
}

#[test]
fn runtime_dependency_edges_exclude_dev_build_and_external_dependencies() {
    let metadata = serde_json::json!({
        "packages": [
            {
                "name": "gateway-api",
                "dependencies": [
                    { "name": "gateway-core", "kind": null },
                    { "name": "gateway-store", "kind": "dev" },
                    { "name": "gateway-host", "kind": "build" },
                    { "name": "serde", "kind": null }
                ]
            },
            {
                "name": "external-package",
                "dependencies": [{ "name": "gateway-core", "kind": null }]
            }
        ]
    });

    assert_eq!(
        workspace_runtime_dependency_edges(&metadata),
        vec![("gateway-api".to_owned(), "gateway-core".to_owned())]
    );
}

fn workspace_runtime_dependency_edges(metadata: &serde_json::Value) -> Vec<(String, String)> {
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
            if dependency.get("kind").expect("dependency kind").is_null() {
                actual.push((name.to_owned(), dep_name.to_owned()));
            }
        }
    }

    actual.sort();
    actual
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
fn workspace_modules_follow_conventional_file_layout() {
    let targets = test_source_roots();
    for member in WORKSPACE_MEMBERS {
        let member_root = backend_root().join(member);
        let src = member_root.join("src");
        assert_module_tree(&src, &["lib.rs", "main.rs"]);
        assert_directory_modules_have_children(&src, &["lib.rs", "main.rs"]);

        let tests = member_root.join("tests");
        if tests.is_dir() {
            let roots = targets.get(&tests).expect("Cargo test source roots");
            assert_module_tree(
                &tests,
                &roots.iter().map(String::as_str).collect::<Vec<_>>(),
            );
        }
    }
}

fn assert_directory_modules_have_children(root: &Path, crate_roots: &[&str]) {
    let files = super::rust_files(root);
    for relative in &files {
        if crate_roots
            .iter()
            .any(|candidate| relative == Path::new(candidate))
            || relative.file_name().and_then(|value| value.to_str()) != Some("mod.rs")
        {
            continue;
        }

        let directory = relative.parent().expect("mod.rs parent");
        assert!(
            files
                .iter()
                .any(|candidate| candidate != relative && candidate.starts_with(directory)),
            "{} is a leaf module and must use {}",
            root.join(relative).display(),
            root.join(directory).with_extension("rs").display(),
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModuleLayout {
    File,
    Directory,
}

#[test]
fn integration_tests_mirror_production_module_tree() {
    let targets = test_source_roots();
    for member in WORKSPACE_MEMBERS {
        let member_root = backend_root().join(member);
        let tests_root = member_root.join("tests");
        if !tests_root.is_dir() {
            continue;
        }

        let production = module_layouts(&member_root.join("src"), &["lib.rs", "main.rs"]);
        let roots = targets.get(&tests_root).expect("Cargo test source roots");
        let tests = module_layouts(
            &tests_root,
            &roots.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        for (module, test_layout) in tests {
            if module.starts_with("support") {
                continue;
            }

            if let Some(production_layout) = production.get(&module) {
                assert_eq!(
                    test_layout,
                    *production_layout,
                    "{} must mirror the production module layout for {}",
                    tests_root.join(module.with_extension("rs")).display(),
                    member_root.join("src").join(&module).display(),
                );
                continue;
            }

            let has_production_owner = module
                .parent()
                .into_iter()
                .flat_map(Path::ancestors)
                .take_while(|ancestor| !ancestor.as_os_str().is_empty())
                .any(|ancestor| production.contains_key(ancestor));
            assert!(
                has_production_owner || root_test_scenario_allowed(member, &module),
                "{} has no mirrored production module owner",
                tests_root.join(module.with_extension("rs")).display(),
            );
        }
    }
}

/// Rust 子进程 fixture 是 Cargo 的独立 crate 根，不能要求它由测试模块再次声明
fn test_source_roots() -> BTreeMap<PathBuf, Vec<String>> {
    let metadata = cargo_metadata_json();
    let mut roots = BTreeMap::<PathBuf, Vec<String>>::new();
    for package in metadata["packages"].as_array().expect("Cargo packages") {
        let root = Path::new(package["manifest_path"].as_str().expect("package manifest"))
            .parent()
            .expect("package directory")
            .join("tests");
        for target in package["targets"].as_array().expect("Cargo targets") {
            let source = Path::new(target["src_path"].as_str().expect("target source"));
            if let Ok(relative) = source.strip_prefix(&root) {
                roots
                    .entry(root.clone())
                    .or_default()
                    .push(relative.to_str().expect("UTF-8 source path").to_owned());
            }
        }
    }
    roots
}

fn module_layouts(root: &Path, crate_roots: &[&str]) -> BTreeMap<PathBuf, ModuleLayout> {
    super::rust_files(root)
        .into_iter()
        .filter(|relative| {
            !crate_roots
                .iter()
                .any(|candidate| relative == Path::new(candidate))
        })
        .map(|relative| {
            if relative.file_name().and_then(|value| value.to_str()) == Some("mod.rs") {
                (
                    relative.parent().expect("mod.rs parent").to_path_buf(),
                    ModuleLayout::Directory,
                )
            } else {
                (relative.with_extension(""), ModuleLayout::File)
            }
        })
        .collect()
}

fn root_test_scenario_allowed(member: &str, module: &Path) -> bool {
    let Some(module) = module.to_str() else {
        return false;
    };
    ROOT_TEST_SCENARIOS
        .iter()
        .any(|(candidate, scenarios)| *candidate == member && scenarios.contains(&module))
}

fn assert_module_tree(root: &Path, crate_roots: &[&str]) {
    let files = super::rust_files(root);
    // 同一父模块的所有子文件复用一次语法解析，计数仍保留重复声明检查
    let mut declarations = BTreeMap::<PathBuf, BTreeMap<String, usize>>::new();
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
            .map(|path| {
                declarations
                    .entry(path.clone())
                    .or_insert_with(|| external_module_declarations(path))
                    .get(module_name)
                    .copied()
                    .unwrap_or_default()
            })
            .sum::<usize>();
        assert_eq!(
            declaration_count,
            1,
            "{} must be declared exactly once by its parent module",
            root.join(relative).display()
        );
    }
}

fn external_module_declarations(path: &Path) -> BTreeMap<String, usize> {
    if !path.is_file() {
        return BTreeMap::new();
    }
    let source = fs::read_to_string(path).expect("read parent module source");
    let syntax = syn::parse_file(&source).expect("parse parent module source");
    let mut declarations = BTreeMap::new();
    for item in syntax.items {
        if let Item::Mod(module) = item
            && module.content.is_none()
        {
            *declarations.entry(module.ident.to_string()).or_default() += 1;
        }
    }
    declarations
}

pub(super) fn backend_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("backend workspace root")
}

fn cargo_metadata_json() -> &'static serde_json::Value {
    static METADATA: OnceLock<serde_json::Value> = OnceLock::new();
    METADATA.get_or_init(|| {
        let output = std::process::Command::new("cargo")
            .args(["metadata", "--format-version", "1", "--no-deps", "--locked"])
            .current_dir(backend_root())
            .output()
            .expect("run cargo metadata");
        assert!(
            output.status.success(),
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("parse cargo metadata")
    })
}

/// 复用 Cargo 解析后的实际包名，覆盖重命名依赖与平台条件，排除测试和构建依赖
fn dependency_names(member: &str) -> Vec<String> {
    let manifest = backend_root().join(member).join("Cargo.toml");
    let package = cargo_metadata_json()["packages"]
        .as_array()
        .expect("Cargo packages")
        .iter()
        .find(|package| {
            Path::new(package["manifest_path"].as_str().expect("package manifest")) == manifest
        })
        .expect("workspace member");
    package["dependencies"]
        .as_array()
        .expect("package dependencies")
        .iter()
        .filter(|dependency| dependency["kind"].is_null())
        .map(|dependency| {
            dependency["name"]
                .as_str()
                .expect("dependency name")
                .to_owned()
        })
        .collect()
}

fn assert_no_dependency(member: &str, forbidden: &[&str]) {
    for name in dependency_names(member) {
        assert!(
            !forbidden.contains(&name.as_str()),
            "{member} must not depend on `{name}`"
        );
    }
}
