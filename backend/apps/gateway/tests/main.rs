mod architecture;
mod bootstrap;

// 组合根是否混入业务策略由代码审查判断，行数和标识符/注释关键词不能证明职责越界。
// 依赖 DAG、公开模块面和模块镜像继续由 architecture 检查，源码纪律由下方测试维护。

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use syn::{Attribute, Item, Meta};

#[test]
fn app_tree_matches_frozen_terminal_manifest() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(
        rust_files(&root.join("src")),
        BTreeSet::from([
            PathBuf::from("bootstrap.rs"),
            PathBuf::from("lib.rs"),
            PathBuf::from("main.rs"),
        ]),
    );
    assert_eq!(
        rust_files(&root.join("tests")),
        BTreeSet::from([
            PathBuf::from("architecture.rs"),
            PathBuf::from("bootstrap.rs"),
            PathBuf::from("main.rs"),
        ]),
    );
}

#[test]
fn cargo_library_root_is_conventional_lib() {
    let manifest = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .expect("read app Cargo.toml");
    assert!(!manifest.contains("[lib]"));
    assert!(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/lib.rs")
            .is_file()
    );
}

#[test]
fn workspace_production_files_have_no_hidden_modules_or_test_hooks() {
    for member in architecture::WORKSPACE_MEMBERS {
        let root = architecture::backend_root().join(member).join("src");
        for relative in rust_files(&root) {
            let path = root.join(relative);
            let source = fs::read_to_string(&path).expect("read production source");
            assert!(
                !source.contains("include!("),
                "{} uses include!",
                path.display()
            );
            let syntax = syn::parse_file(&source).expect("parse production source");
            for item in &syntax.items {
                if let Item::Mod(module) = item {
                    assert!(
                        module.content.is_none(),
                        "{} has an inline module",
                        path.display()
                    );
                }
                for attribute in item_attrs(item) {
                    assert!(
                        !is_path_or_test_cfg(attribute),
                        "{} has a production test/path hook",
                        path.display(),
                    );
                }
            }
        }
    }
}

fn rust_files(root: &Path) -> BTreeSet<PathBuf> {
    let mut result = BTreeSet::new();
    collect_rust_files(root, root, &mut result);
    result
}

fn collect_rust_files(root: &Path, current: &Path, result: &mut BTreeSet<PathBuf>) {
    for entry in fs::read_dir(current).expect("read app tree") {
        let path = entry.expect("read app tree entry").path();
        if path.is_dir() {
            collect_rust_files(root, &path, result);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            result.insert(
                path.strip_prefix(root)
                    .expect("relative app path")
                    .to_path_buf(),
            );
        }
    }
}

fn is_path_or_test_cfg(attribute: &Attribute) -> bool {
    if attribute.path().is_ident("path") {
        return true;
    }
    if !attribute.path().is_ident("cfg") && !attribute.path().is_ident("cfg_attr") {
        return false;
    }
    let Meta::List(list) = &attribute.meta else {
        return false;
    };
    list.tokens
        .to_string()
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|segment| segment == "test" || segment.starts_with("test_"))
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        _ => &[],
    }
}
