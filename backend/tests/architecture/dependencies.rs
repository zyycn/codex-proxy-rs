use std::fs;
use std::path::{Path, PathBuf};

use syn::visit::Visit;
use syn::{ItemUse, UseTree};

#[test]
fn global_module_dependencies_follow_architecture_boundaries() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rust_files(&source_root, &mut files);

    let mut violations = Vec::new();
    for file in files {
        let module = module_path(&source_root, &file);
        let source = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
        let syntax = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("parse {}: {error}", file.display()));
        let mut visitor = DependencyVisitor::default();
        visitor.visit_file(&syntax);

        for path in visitor.paths {
            let dependency = resolve_path(&module, &path);
            if let Some(rule) = forbidden_dependency(&module, &dependency) {
                violations.push(format!(
                    "{}: {} -> {} ({rule})",
                    file.strip_prefix(&source_root).unwrap().display(),
                    display_path(&module),
                    display_path(&dependency),
                ));
            }
        }
    }

    violations.sort();
    violations.dedup();
    assert!(
        violations.is_empty(),
        "forbidden architecture dependencies:\n{}",
        violations.join("\n")
    );
}

#[derive(Default)]
struct DependencyVisitor {
    paths: Vec<Vec<String>>,
}

impl<'ast> Visit<'ast> for DependencyVisitor {
    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        collect_use_tree(&item.tree, &mut Vec::new(), &mut self.paths);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        self.paths.push(
            path.segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect(),
        );
        syn::visit::visit_path(self, path);
    }
}

fn collect_use_tree(tree: &UseTree, prefix: &mut Vec<String>, paths: &mut Vec<Vec<String>>) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            collect_use_tree(&path.tree, prefix, paths);
            prefix.pop();
        }
        UseTree::Name(name) => {
            let mut path = prefix.clone();
            path.push(name.ident.to_string());
            paths.push(path);
        }
        UseTree::Rename(rename) => {
            let mut path = prefix.clone();
            path.push(rename.ident.to_string());
            paths.push(path);
        }
        UseTree::Glob(_) => paths.push(prefix.clone()),
        UseTree::Group(group) => {
            for item in &group.items {
                collect_use_tree(item, prefix, paths);
            }
        }
    }
}

fn collect_rust_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read directory {}: {error}", directory.display()))
    {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

fn module_path(source_root: &Path, file: &Path) -> Vec<String> {
    let relative = file.strip_prefix(source_root).expect("source path");
    let mut segments: Vec<_> = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    let file_name = segments.pop().expect("Rust source filename");
    if file_name != "lib.rs" && file_name != "main.rs" && file_name != "mod.rs" {
        segments.push(file_name.trim_end_matches(".rs").to_string());
    }
    segments
}

fn resolve_path(module: &[String], path: &[String]) -> Vec<String> {
    let Some(first) = path.first() else {
        return Vec::new();
    };

    if first == "crate" {
        return path[1..].to_vec();
    }

    if first == "self" {
        return module.iter().chain(&path[1..]).cloned().collect();
    }

    if first == "super" {
        let mut resolved = module.to_vec();
        let mut index = 0;
        while path.get(index).is_some_and(|segment| segment == "super") {
            resolved.pop();
            index += 1;
        }
        resolved.extend_from_slice(&path[index..]);
        return resolved;
    }

    if path.len() == 1 {
        return Vec::new();
    }

    path.to_vec()
}

fn forbidden_dependency(source: &[String], dependency: &[String]) -> Option<&'static str> {
    let source_owner = source.first()?.as_str();
    let dependency_owner = dependency.first().map(String::as_str);

    if source_owner == "fleet" && matches!(dependency_owner, Some("telemetry" | "upstream")) {
        return Some("fleet may only consume its own ports and domain values");
    }

    if source_owner == "models" && dependency_owner == Some("upstream") {
        return Some("models owns ModelCatalogSource; upstream is only its adapter");
    }

    if source_owner == "api" && matches!(dependency_owner, Some("sqlx" | "redis")) {
        return Some("API code cannot access database clients directly");
    }

    None
}

fn display_path(path: &[String]) -> String {
    if path.is_empty() {
        return "<root>".to_string();
    }
    path.join("::")
}
