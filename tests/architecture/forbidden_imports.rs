use std::{fs, path::Path};

#[test]
fn source_imports_should_respect_layer_boundaries() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();

    collect_forbidden_imports(root, &mut offenders);

    assert!(
        offenders.is_empty(),
        "forbidden imports or source names found:\n{}",
        offenders.join("\n")
    );
}

fn collect_forbidden_imports(root: &Path, offenders: &mut Vec<String>) {
    for crate_name in ["core", "adapters", "runtime", "platform", "server"] {
        let src = root.join("crates").join(crate_name).join("src");
        collect_crate_imports(crate_name, &src, offenders);
    }
}

fn collect_crate_imports(crate_name: &str, dir: &Path, offenders: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) == Some("tests") {
                continue;
            }
            if crate_name == "core"
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| matches!(name, "repository" | "transport"))
            {
                offenders.push(path.display().to_string());
            }
            collect_crate_imports(crate_name, &path, offenders);
            continue;
        }

        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if crate_name == "core" && file_name.ends_with("_repository.rs") {
            offenders.push(path.display().to_string());
        }
        if crate_name == "server" && file_name == "facade.rs" {
            offenders.push(path.display().to_string());
        }

        let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
            continue;
        };
        if extension != "rs" {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        for forbidden in forbidden_patterns(crate_name) {
            if content.contains(forbidden) {
                offenders.push(format!("{} contains `{forbidden}`", path.display()));
            }
        }
    }
}

fn forbidden_patterns(crate_name: &str) -> &'static [&'static str] {
    match crate_name {
        "core" => &[
            "use axum::",
            "axum::",
            "use sqlx::",
            "sqlx::",
            "use reqwest::",
            "reqwest::",
            "use tokio_tungstenite::",
            "tokio_tungstenite::",
            "use tungstenite::",
            "tungstenite::",
            "use rustls::",
            "rustls::",
            "use tokio_rustls::",
            "tokio_rustls::",
            "std::fs",
            "std::env",
            "dirs::",
            "std::path",
            "PathBuf",
            "Path::",
        ],
        "adapters" | "platform" => &["use axum::", "axum::"],
        "runtime" => &[
            "use axum::",
            "axum::",
            "use sqlx::",
            "sqlx::",
            "use reqwest::",
            "reqwest::",
            "use tokio_tungstenite::",
            "tokio_tungstenite::",
            "use tungstenite::",
            "tungstenite::",
        ],
        "server" => &[
            "use sqlx::",
            "sqlx::",
            "use reqwest::",
            "reqwest::",
            "use tokio_tungstenite::",
            "tokio_tungstenite::",
            "use tungstenite::",
            "tungstenite::",
        ],
        _ => &[],
    }
}
