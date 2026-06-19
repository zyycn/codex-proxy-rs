use std::{fs, path::Path};

#[test]
fn rust_sources_should_not_keep_placeholder_markers() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();

    collect_placeholder_markers(&root.join("src"), &mut offenders);
    collect_placeholder_markers(&root.join("crates"), &mut offenders);

    assert!(
        offenders.is_empty(),
        "placeholder implementation markers found:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn serving_modules_should_not_keep_uncalled_toy_helpers() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();

    for (relative_path, marker) in [
        (
            "crates/core/src/serving/fallback.rs",
            "pub fn should_fallback(",
        ),
        (
            "crates/core/src/serving/recovery.rs",
            "pub fn is_recoverable(",
        ),
        (
            "crates/core/src/serving/responses.rs",
            "pub fn prefers_websocket(",
        ),
    ] {
        let path = root.join(relative_path);
        let content = fs::read_to_string(&path).unwrap_or_default();
        if content.contains(marker) {
            offenders.push(format!("{relative_path} keeps toy helper `{marker}`"));
        }
    }

    if production_call_count(root, "quota_reached(", "crates/core/src/serving/quota.rs") == 0 {
        offenders.push(
            "crates/core/src/serving/quota.rs exposes `quota_reached` without a production caller"
                .to_string(),
        );
    }

    assert!(
        offenders.is_empty(),
        "uncalled serving toy helpers found:\n{}",
        offenders.join("\n")
    );
}

fn collect_placeholder_markers(dir: &Path, offenders: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_placeholder_markers(&path, offenders);
            continue;
        }

        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }

        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };

        for marker in [
            "fn main() {}",
            "not wired yet",
            "后续承载",
            "pub struct CodexWebSocketConnection;",
            "pub struct CodexWebSocketPool;",
        ] {
            if content.contains(marker) {
                offenders.push(format!("{} contains `{marker}`", path.display()));
            }
        }
    }
}

fn production_call_count(root: &Path, needle: &str, defining_file: &str) -> usize {
    let mut count = 0;
    count_production_calls(
        &root.join("crates"),
        needle,
        &root.join(defining_file),
        &mut count,
    );
    count
}

fn count_production_calls(dir: &Path, needle: &str, defining_file: &Path, count: &mut usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) == Some("tests") {
                continue;
            }
            count_production_calls(&path, needle, defining_file, count);
            continue;
        }
        if path == defining_file {
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        *count += content.matches(needle).count();
    }
}
