use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn rust_source_tree_should_match_architecture_whitelist() {
    let root = repository_root();
    let expected = expected_rust_source_files();
    let actual = actual_rust_source_files(&root);

    assert_eq!(
        actual,
        expected,
        "Rust source tree must match docs/architecture.md exactly.\nmissing:\n{}\nextra:\n{}",
        format_missing(&expected, &actual),
        format_extra(&expected, &actual)
    );
}

#[test]
fn rust_source_directories_should_match_architecture_whitelist() {
    let root = repository_root();
    let expected = expected_rust_source_directories();
    let actual = actual_rust_source_directories(&root);

    assert_eq!(
        actual,
        expected,
        "Rust source directories must match docs/architecture.md exactly.\nmissing:\n{}\nextra:\n{}",
        format_missing(&expected, &actual),
        format_extra(&expected, &actual)
    );
}

#[test]
fn workspace_members_should_match_architecture_crates() {
    let cargo_toml = fs::read_to_string(repository_root().join("Cargo.toml"))
        .expect("expected root Cargo.toml to be readable");

    assert!(
        cargo_toml.contains("members = [\"crates/*\"]"),
        "root workspace should keep members = [\"crates/*\"]"
    );
}

fn actual_rust_source_files(root: &Path) -> BTreeSet<String> {
    let mut files = BTreeSet::new();
    collect_rust_source_files(&root.join("src"), root, &mut files);
    let crates_dir = root.join("crates");
    let entries = fs::read_dir(&crates_dir).expect("expected crates directory to be readable");
    for entry in entries.flatten() {
        let path = entry.path().join("src");
        collect_rust_source_files(&path, root, &mut files);
    }
    files
}

fn actual_rust_source_directories(root: &Path) -> BTreeSet<String> {
    let mut directories = BTreeSet::new();
    collect_rust_source_directories(&root.join("src"), root, &mut directories);
    let crates_dir = root.join("crates");
    let entries = fs::read_dir(&crates_dir).expect("expected crates directory to be readable");
    for entry in entries.flatten() {
        let path = entry.path().join("src");
        collect_rust_source_directories(&path, root, &mut directories);
    }
    directories
}

fn collect_rust_source_files(dir: &Path, root: &Path, files: &mut BTreeSet<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_source_files(&path, root, files);
            continue;
        }

        let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
            continue;
        };

        if extension == "rs" || extension == "sql" {
            let relative = path
                .strip_prefix(root)
                .expect("source file should be below repository root")
                .to_string_lossy()
                .replace('\\', "/");
            files.insert(relative);
        }
    }
}

fn collect_rust_source_directories(dir: &Path, root: &Path, directories: &mut BTreeSet<String>) {
    if !dir.is_dir() {
        return;
    }

    let relative = dir
        .strip_prefix(root)
        .expect("source directory should be below repository root")
        .to_string_lossy()
        .replace('\\', "/");
    directories.insert(relative);

    let entries = fs::read_dir(dir).expect("expected source directory to be readable");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_source_directories(&path, root, directories);
        }
    }
}

fn format_missing(expected: &BTreeSet<String>, actual: &BTreeSet<String>) -> String {
    expected
        .difference(actual)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_extra(expected: &BTreeSet<String>, actual: &BTreeSet<String>) -> String {
    actual
        .difference(expected)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

fn expected_rust_source_files() -> BTreeSet<String> {
    [
        "crates/core/src/lib.rs",
        "crates/core/src/error.rs",
        "crates/core/src/admin/mod.rs",
        "crates/core/src/admin/ports.rs",
        "crates/core/src/admin/auth.rs",
        "crates/core/src/admin/client_keys.rs",
        "crates/core/src/admin/settings.rs",
        "crates/core/src/accounts/mod.rs",
        "crates/core/src/accounts/model.rs",
        "crates/core/src/accounts/ports.rs",
        "crates/core/src/accounts/service.rs",
        "crates/core/src/accounts/pool.rs",
        "crates/core/src/accounts/lifecycle.rs",
        "crates/core/src/accounts/cloudflare.rs",
        "crates/core/src/accounts/cookies.rs",
        "crates/core/src/accounts/jwt.rs",
        "crates/core/src/accounts/usage.rs",
        "crates/core/src/auth/mod.rs",
        "crates/core/src/auth/ports.rs",
        "crates/core/src/auth/oauth.rs",
        "crates/core/src/auth/session.rs",
        "crates/core/src/models/mod.rs",
        "crates/core/src/models/model.rs",
        "crates/core/src/models/ports.rs",
        "crates/core/src/models/catalog.rs",
        "crates/core/src/models/service.rs",
        "crates/core/src/events/mod.rs",
        "crates/core/src/events/model.rs",
        "crates/core/src/events/ports.rs",
        "crates/core/src/events/service.rs",
        "crates/core/src/usage/mod.rs",
        "crates/core/src/usage/model.rs",
        "crates/core/src/usage/ports.rs",
        "crates/core/src/usage/service.rs",
        "crates/core/src/protocol/mod.rs",
        "crates/core/src/protocol/openai/mod.rs",
        "crates/core/src/protocol/openai/chat.rs",
        "crates/core/src/protocol/openai/responses.rs",
        "crates/core/src/protocol/openai/models.rs",
        "crates/core/src/protocol/openai/errors.rs",
        "crates/core/src/protocol/codex/mod.rs",
        "crates/core/src/protocol/codex/chat.rs",
        "crates/core/src/protocol/codex/responses.rs",
        "crates/core/src/protocol/codex/events.rs",
        "crates/core/src/protocol/codex/sse.rs",
        "crates/core/src/protocol/codex/websocket.rs",
        "crates/core/src/protocol/codex/schema.rs",
        "crates/core/src/gateway/mod.rs",
        "crates/core/src/gateway/ports.rs",
        "crates/core/src/gateway/fingerprint.rs",
        "crates/core/src/gateway/conversation.rs",
        "crates/core/src/gateway/installation.rs",
        "crates/core/src/serving/mod.rs",
        "crates/core/src/serving/chat.rs",
        "crates/core/src/serving/responses.rs",
        "crates/core/src/serving/errors.rs",
        "crates/core/src/serving/routing.rs",
        "crates/core/src/serving/fallback.rs",
        "crates/core/src/serving/affinity.rs",
        "crates/core/src/serving/quota.rs",
        "crates/core/src/serving/implicit_resume.rs",
        "crates/core/src/serving/reasoning_replay.rs",
        "crates/core/src/serving/stream.rs",
        "crates/core/src/serving/recovery.rs",
        "crates/core/src/serving/usage.rs",
        "crates/adapters/src/lib.rs",
        "crates/adapters/src/sqlite/mod.rs",
        "crates/adapters/src/sqlite/accounts.rs",
        "crates/adapters/src/sqlite/account_tokens.rs",
        "crates/adapters/src/sqlite/account_usage.rs",
        "crates/adapters/src/sqlite/refresh_leases.rs",
        "crates/adapters/src/sqlite/cookies.rs",
        "crates/adapters/src/sqlite/events.rs",
        "crates/adapters/src/sqlite/models.rs",
        "crates/adapters/src/sqlite/session_affinity.rs",
        "crates/adapters/src/sqlite/admin_sessions.rs",
        "crates/adapters/src/sqlite/client_keys.rs",
        "crates/adapters/src/codex/mod.rs",
        "crates/adapters/src/codex/client.rs",
        "crates/adapters/src/codex/models.rs",
        "crates/adapters/src/codex/fingerprint.rs",
        "crates/adapters/src/codex/websocket/mod.rs",
        "crates/adapters/src/codex/websocket/connect.rs",
        "crates/adapters/src/codex/websocket/pool.rs",
        "crates/adapters/src/codex/websocket/deflate.rs",
        "crates/adapters/src/codex/websocket/opening.rs",
        "crates/adapters/src/oauth/mod.rs",
        "crates/adapters/src/oauth/openai.rs",
        "crates/runtime/src/lib.rs",
        "crates/runtime/src/bootstrap.rs",
        "crates/runtime/src/state.rs",
        "crates/runtime/src/services.rs",
        "crates/runtime/src/repositories.rs",
        "crates/runtime/src/upstream.rs",
        "crates/runtime/src/config.rs",
        "crates/runtime/src/tasks/mod.rs",
        "crates/runtime/src/tasks/coordinator.rs",
        "crates/runtime/src/tasks/token_refresh.rs",
        "crates/runtime/src/tasks/quota_refresh.rs",
        "crates/runtime/src/tasks/model_refresh.rs",
        "crates/runtime/src/tasks/cookie_cleanup.rs",
        "crates/runtime/src/tasks/session_cleanup.rs",
        "crates/runtime/src/tasks/session_affinity_cleanup.rs",
        "crates/runtime/src/tasks/fingerprint_update.rs",
        "crates/server/src/main.rs",
        "crates/server/src/lib.rs",
        "crates/server/src/router.rs",
        "crates/server/src/error/mod.rs",
        "crates/server/src/error/admin.rs",
        "crates/server/src/error/openai.rs",
        "crates/server/src/middleware/mod.rs",
        "crates/server/src/middleware/request_id.rs",
        "crates/server/src/middleware/trace.rs",
        "crates/server/src/middleware/auth.rs",
        "crates/server/src/middleware/cors.rs",
        "crates/server/src/admin_api/mod.rs",
        "crates/server/src/admin_api/router.rs",
        "crates/server/src/admin_api/response.rs",
        "crates/server/src/admin_api/session.rs",
        "crates/server/src/admin_api/settings.rs",
        "crates/server/src/admin_api/diagnostics.rs",
        "crates/server/src/admin_api/models.rs",
        "crates/server/src/admin_api/usage.rs",
        "crates/server/src/admin_api/accounts/mod.rs",
        "crates/server/src/admin_api/accounts/list.rs",
        "crates/server/src/admin_api/accounts/create.rs",
        "crates/server/src/admin_api/accounts/import.rs",
        "crates/server/src/admin_api/accounts/import_cli.rs",
        "crates/server/src/admin_api/accounts/export.rs",
        "crates/server/src/admin_api/accounts/lifecycle.rs",
        "crates/server/src/admin_api/accounts/quota.rs",
        "crates/server/src/admin_api/accounts/cookies.rs",
        "crates/server/src/admin_api/accounts/oauth.rs",
        "crates/server/src/admin_api/accounts/health.rs",
        "crates/server/src/admin_api/client_keys/mod.rs",
        "crates/server/src/admin_api/client_keys/list.rs",
        "crates/server/src/admin_api/client_keys/create.rs",
        "crates/server/src/admin_api/client_keys/import.rs",
        "crates/server/src/admin_api/client_keys/export.rs",
        "crates/server/src/admin_api/client_keys/lifecycle.rs",
        "crates/server/src/admin_api/logs/mod.rs",
        "crates/server/src/admin_api/logs/query.rs",
        "crates/server/src/admin_api/logs/detail.rs",
        "crates/server/src/admin_api/logs/state.rs",
        "crates/server/src/openai_api/mod.rs",
        "crates/server/src/openai_api/router.rs",
        "crates/server/src/openai_api/auth.rs",
        "crates/server/src/openai_api/chat.rs",
        "crates/server/src/openai_api/responses.rs",
        "crates/server/src/openai_api/models.rs",
        "crates/server/src/openai_api/diagnostics.rs",
        "crates/server/src/openai_api/error.rs",
        "crates/server/src/openai_api/sse.rs",
        "crates/platform/src/lib.rs",
        "crates/platform/src/config/mod.rs",
        "crates/platform/src/config/loader.rs",
        "crates/platform/src/config/types.rs",
        "crates/platform/src/crypto/mod.rs",
        "crates/platform/src/crypto/secret_box.rs",
        "crates/platform/src/crypto/hash.rs",
        "crates/platform/src/identity/mod.rs",
        "crates/platform/src/identity/admin_password.rs",
        "crates/platform/src/identity/client_key.rs",
        "crates/platform/src/storage/mod.rs",
        "crates/platform/src/storage/sqlite.rs",
        "crates/platform/src/storage/schema.sql",
        "crates/platform/src/storage/paths.rs",
        "crates/platform/src/logging/mod.rs",
        "crates/platform/src/logging/rotation.rs",
        "crates/platform/src/json/mod.rs",
        "crates/platform/src/json/pagination.rs",
        "crates/assets/src/lib.rs",
        "crates/assets/src/router.rs",
        "crates/assets/src/headers.rs",
        "crates/xtask/src/main.rs",
        "crates/xtask/src/build_web.rs",
        "crates/xtask/src/check_architecture.rs",
        "crates/xtask/src/release.rs",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

fn expected_rust_source_directories() -> BTreeSet<String> {
    [
        "crates/core/src",
        "crates/core/src/admin",
        "crates/core/src/accounts",
        "crates/core/src/auth",
        "crates/core/src/models",
        "crates/core/src/events",
        "crates/core/src/usage",
        "crates/core/src/protocol",
        "crates/core/src/protocol/openai",
        "crates/core/src/protocol/codex",
        "crates/core/src/gateway",
        "crates/core/src/serving",
        "crates/adapters/src",
        "crates/adapters/src/sqlite",
        "crates/adapters/src/codex",
        "crates/adapters/src/codex/websocket",
        "crates/adapters/src/oauth",
        "crates/runtime/src",
        "crates/runtime/src/tasks",
        "crates/server/src",
        "crates/server/src/error",
        "crates/server/src/middleware",
        "crates/server/src/admin_api",
        "crates/server/src/admin_api/accounts",
        "crates/server/src/admin_api/client_keys",
        "crates/server/src/admin_api/logs",
        "crates/server/src/openai_api",
        "crates/platform/src",
        "crates/platform/src/config",
        "crates/platform/src/crypto",
        "crates/platform/src/identity",
        "crates/platform/src/storage",
        "crates/platform/src/logging",
        "crates/platform/src/json",
        "crates/assets/src",
        "crates/xtask/src",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}
