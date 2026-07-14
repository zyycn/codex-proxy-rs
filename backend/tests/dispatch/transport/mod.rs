use std::{fs, path::Path};

#[test]
fn transport_must_normalize_facts_without_owning_feature_decisions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/dispatch/transport");

    for path in rust_files(&root) {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        for forbidden in [
            "ControllerSet",
            "controllers::",
            "AttemptDecision",
            "RetryNextCandidate",
            "RetrySameCandidate",
            "cyber_policy",
        ] {
            assert!(
                !source.contains(forbidden),
                "transport adapter {} must emit typed facts instead of owning {forbidden}",
                path.display()
            );
        }
    }
}

#[test]
fn canonical_stream_decoder_must_be_the_only_dispatch_sse_parser() {
    let dispatch = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/dispatch");
    let canonical = dispatch.join("transport/canonical.rs");
    let canonical_source = fs::read_to_string(&canonical).expect("canonical transport source");
    assert!(canonical_source.contains("parse_sse_events"));
    assert!(canonical_source.contains("fn normalize_complete_response"));
    let protocol = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/upstream/openai/protocol/responses.rs"),
    )
    .expect("responses protocol source");
    assert!(!protocol.contains("fn response_from_codex_sse"));

    for path in rust_files(&dispatch) {
        if path == canonical {
            continue;
        }
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        assert!(
            !source.contains("parse_sse_events"),
            "{} must consume canonical events instead of decoding SSE again",
            path.display()
        );
    }
}

fn rust_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    collect_rust_files(root, &mut files);
    files
}

fn collect_rust_files(path: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(path).expect("source directory should be readable") {
        let entry = entry.expect("source entry should be readable");
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}
