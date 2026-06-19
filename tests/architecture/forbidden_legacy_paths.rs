use std::{fs, path::PathBuf};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn source_tree_should_not_reference_legacy_gateway_oauth_module() {
    let root = repository_root();
    let mut offenders = Vec::new();

    for relative_dir in ["src", "tests"] {
        let dir = root.join(relative_dir);
        collect_legacy_oauth_references(&dir, &mut offenders);
    }

    assert!(
        offenders.is_empty(),
        "legacy gateway::oauth references should be removed:\n{}",
        offenders.join("\n")
    );
}

fn collect_legacy_oauth_references(dir: &PathBuf, offenders: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_legacy_oauth_references(&path, offenders);
            continue;
        }

        let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        if extension != "rs" {
            continue;
        }

        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        if path.ends_with("tests/architecture/forbidden_legacy_paths.rs") {
            continue;
        }

        if content.contains("gateway::oauth") {
            offenders.push(path.display().to_string());
        }
    }
}
