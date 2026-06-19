use std::{fs, path::PathBuf};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn workspace_should_declare_crates_glob_member() {
    let cargo_toml = fs::read_to_string(repository_root().join("Cargo.toml"))
        .expect("expected root Cargo.toml to be readable");

    assert!(
        cargo_toml.contains("[workspace]") && cargo_toml.contains("members = [\"crates/*\"]"),
        "root Cargo.toml should declare a workspace with members = [\"crates/*\"]"
    );
}

#[test]
fn workspace_should_scaffold_required_member_crates() {
    let root = repository_root();

    for crate_name in [
        "core", "adapters", "runtime", "server", "platform", "assets", "xtask",
    ] {
        let crate_dir = root.join("crates").join(crate_name);
        assert!(
            crate_dir.join("Cargo.toml").is_file(),
            "expected {crate_name} crate to define Cargo.toml at {}",
            crate_dir.join("Cargo.toml").display()
        );

        let entry_paths = if crate_name == "server" {
            vec![crate_dir.join("src/lib.rs"), crate_dir.join("src/main.rs")]
        } else if crate_name == "xtask" {
            vec![crate_dir.join("src/main.rs")]
        } else {
            vec![crate_dir.join("src/lib.rs")]
        };

        for entry_path in entry_paths {
            assert!(
                entry_path.is_file(),
                "expected {crate_name} crate to define source entry at {}",
                entry_path.display()
            );
        }
    }
}

#[test]
fn workspace_should_scaffold_runtime_server_and_assets_shapes() {
    let root = repository_root();

    for relative_path in [
        "crates/core/src/admin/mod.rs",
        "crates/core/src/admin/ports.rs",
        "crates/core/src/admin/client_keys.rs",
        "crates/core/src/accounts/model.rs",
        "crates/core/src/auth/mod.rs",
        "crates/core/src/auth/oauth.rs",
        "crates/core/src/gateway/ports.rs",
        "crates/core/src/models/mod.rs",
        "crates/core/src/models/model.rs",
        "crates/core/src/models/catalog.rs",
        "crates/core/src/models/ports.rs",
        "crates/core/src/models/service.rs",
        "crates/core/src/protocol/codex/events.rs",
        "crates/adapters/src/codex/mod.rs",
        "crates/adapters/src/codex/client.rs",
        "crates/adapters/src/codex/models.rs",
        "crates/adapters/src/codex/fingerprint.rs",
        "crates/adapters/src/codex/websocket/mod.rs",
        "crates/adapters/src/codex/websocket/connect.rs",
        "crates/adapters/src/codex/websocket/pool.rs",
        "crates/adapters/src/codex/websocket/deflate.rs",
        "crates/adapters/src/codex/websocket/opening.rs",
        "crates/adapters/src/sqlite/mod.rs",
        "crates/adapters/src/sqlite/client_keys.rs",
        "crates/adapters/src/sqlite/models.rs",
        "crates/assets/src/router.rs",
        "crates/assets/src/headers.rs",
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
        "crates/server/src/openai_api/mod.rs",
        "crates/server/src/openai_api/auth.rs",
        "crates/server/src/openai_api/chat.rs",
        "crates/server/src/openai_api/diagnostics.rs",
        "crates/server/src/openai_api/error.rs",
        "crates/server/src/openai_api/models.rs",
        "crates/server/src/openai_api/responses.rs",
        "crates/server/src/openai_api/router.rs",
        "crates/server/src/openai_api/sse.rs",
        "crates/xtask/src/build_web.rs",
        "crates/xtask/src/check_architecture.rs",
        "crates/xtask/src/release.rs",
    ] {
        assert!(
            root.join(relative_path).is_file(),
            "expected architecture scaffold file at {}",
            root.join(relative_path).display()
        );
    }
}

#[test]
fn migrated_modules_should_not_keep_legacy_duplicates() {
    let root = repository_root();

    for relative_path in [
        "src/main.rs",
        "src/runtime/bootstrap.rs",
        "src/runtime/mod.rs",
        "src/runtime/router.rs",
        "src/runtime/state.rs",
        "src/runtime/tasks/coordinator.rs",
        "src/runtime/tasks/mod.rs",
        "src/runtime/tasks/types.rs",
        "src/codex/accounts/jwt.rs",
        "src/codex/gateway/conversation_identity.rs",
        "src/codex/gateway/installation_id.rs",
        "src/codex/gateway/fingerprint/mod.rs",
        "src/codex/gateway/fingerprint/model.rs",
        "src/codex/gateway/protocol/openai_to_codex.rs",
        "src/codex/gateway/protocol/schema.rs",
        "src/codex/gateway/protocol/tuple_schema.rs",
        "src/codex/gateway/transport/types.rs",
        "src/codex/gateway/transport/endpoints.rs",
        "src/codex/gateway/transport/custom_ca.rs",
        "src/codex/gateway/fingerprint/repository.rs",
        "src/codex/gateway/fingerprint/updater.rs",
        "src/codex/gateway/fingerprint/update_checker.rs",
        "src/codex/gateway/transport/sse.rs",
        "src/codex/gateway/protocol/codex_to_openai.rs",
        "src/codex/gateway/transport/usage_events.rs",
        "src/codex/gateway/transport/retry_after.rs",
        "src/codex/gateway/transport/rate_limits.rs",
        "src/codex/gateway/transport/headers.rs",
        "src/codex/gateway/transport/http_client.rs",
        "src/codex/gateway/oauth/mod.rs",
        "src/codex/gateway/oauth/codex_cli.rs",
        "src/codex/gateway/oauth/client.rs",
        "src/codex/gateway/oauth/token.rs",
        "src/codex/gateway/oauth/refresh.rs",
        "src/codex/models/catalog.rs",
        "src/codex/models/repository.rs",
        "src/codex/models/service.rs",
        "src/codex/serving/http/auth.rs",
        "src/codex/serving/http/chat.rs",
        "src/codex/serving/http/diagnostics.rs",
        "src/codex/serving/http/errors.rs",
        "src/codex/serving/http/mod.rs",
        "src/codex/serving/http/models.rs",
        "src/codex/serving/http/responses.rs",
        "src/codex/serving/http/router.rs",
        "tests/codex_gateway/http_client.rs",
        "tests/codex_models/catalog.rs",
    ] {
        assert!(
            !root.join(relative_path).exists(),
            "migrated legacy module should be removed: {}",
            root.join(relative_path).display()
        );
    }
}

#[test]
fn root_src_should_be_removed_after_workspace_migration() {
    let root = repository_root();

    assert!(
        !root.join("src").exists(),
        "root src should be removed after member crates expose architecture boundaries directly"
    );
}
