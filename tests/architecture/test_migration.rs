use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn root_tests_should_only_keep_architecture_tests_and_shared_fixtures() {
    let root = repository_root();
    let mut offenders = Vec::new();
    collect_root_test_rs_files(&root.join("tests"), &root, &mut offenders);

    assert!(
        offenders.is_empty(),
        "root behavior tests must be migrated into crates/*/tests:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn baseline_behavior_test_suites_should_have_crate_local_migration_coverage() {
    let root = repository_root();
    let current_tests = current_crate_test_files(&root);
    let mut missing = Vec::new();

    for suite in baseline_behavior_suites() {
        for required in suite.required_current_files {
            if !current_tests.contains(*required) {
                missing.push(format!(
                    "{} should be covered by crate-local test file {required}",
                    suite.baseline_path
                ));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "old behavior test suites lack crate-local migration evidence:\n{}",
        missing.join("\n")
    );
}

#[test]
fn core_service_modules_should_have_production_callers() {
    let root = repository_root();
    let mut offenders = Vec::new();

    for service in [
        CoreService {
            path: "crates/core/src/accounts/service.rs",
            symbol: "AccountService::",
        },
        CoreService {
            path: "crates/core/src/usage/service.rs",
            symbol: "UsageService::",
        },
        CoreService {
            path: "crates/core/src/admin/settings.rs",
            symbol: "SettingsService::",
        },
    ] {
        if production_call_count(&root, service.symbol, service.path) == 0 {
            offenders.push(format!(
                "{} exposes {} without a production caller",
                service.path, service.symbol
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "core service modules still look like architecture placeholders:\n{}",
        offenders.join("\n")
    );
}

fn collect_root_test_rs_files(dir: &Path, root: &Path, offenders: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let relative = relative_path(root, &path);
            if relative == "tests/architecture" || relative.starts_with("tests/fixtures") {
                continue;
            }
            collect_root_test_rs_files(&path, root, offenders);
            continue;
        }

        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }

        let relative = relative_path(root, &path);
        if relative != "tests/architecture.rs" && !relative.starts_with("tests/architecture/") {
            offenders.push(relative);
        }
    }
}

fn current_crate_test_files(root: &Path) -> BTreeSet<String> {
    let mut files = BTreeSet::new();
    collect_crate_test_files(&root.join("crates"), root, &mut files);
    files
}

fn collect_crate_test_files(dir: &Path, root: &Path, files: &mut BTreeSet<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_crate_test_files(&path, root, files);
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) == Some("rs")
            && relative_path(root, &path).contains("/tests/")
        {
            files.insert(relative_path(root, &path));
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

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("path should be below repository root")
        .to_string_lossy()
        .replace('\\', "/")
}

struct BaselineSuite {
    baseline_path: &'static str,
    required_current_files: &'static [&'static str],
}

struct CoreService {
    path: &'static str,
    symbol: &'static str,
}

fn baseline_behavior_suites() -> Vec<BaselineSuite> {
    vec![
        BaselineSuite {
            baseline_path: "tests/admin/accounts/cookies_quota.rs",
            required_current_files: &[
                "crates/server/tests/admin_accounts_routes.rs",
                "crates/adapters/tests/cookies.rs",
                "crates/runtime/tests/quota_refresh.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/admin/accounts/import_export.rs",
            required_current_files: &["crates/server/tests/admin_accounts_routes.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/admin/accounts/lifecycle.rs",
            required_current_files: &["crates/server/tests/admin_accounts_routes.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/admin/accounts/list.rs",
            required_current_files: &["crates/server/tests/admin_accounts_routes.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/admin/accounts/oauth.rs",
            required_current_files: &[
                "crates/server/tests/admin_accounts_routes.rs",
                "crates/adapters/tests/oauth.rs",
                "crates/core/tests/auth.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/admin/api_contract.rs",
            required_current_files: &["crates/server/tests/admin_api_contract.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/admin/client_keys_route.rs",
            required_current_files: &[
                "crates/server/tests/admin_client_keys_routes.rs",
                "crates/adapters/tests/client_keys.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/admin/logs_route.rs",
            required_current_files: &[
                "crates/server/tests/admin_logs_routes.rs",
                "crates/adapters/tests/events.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/admin/models_route.rs",
            required_current_files: &[
                "crates/server/tests/admin_models_routes.rs",
                "crates/core/tests/models.rs",
                "crates/adapters/tests/models.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/admin/session.rs",
            required_current_files: &["crates/platform/tests/admin_password.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/admin/session_login_route.rs",
            required_current_files: &["crates/server/tests/admin_session_routes.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/admin/session_repository.rs",
            required_current_files: &["crates/adapters/tests/admin_sessions.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/admin/settings_route.rs",
            required_current_files: &[
                "crates/server/tests/admin_settings_routes.rs",
                "crates/core/tests/admin_settings.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/admin/usage_stats_route.rs",
            required_current_files: &["crates/server/tests/admin_accounts_routes.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/codex_accounts/cookie_store.rs",
            required_current_files: &[
                "crates/adapters/tests/cookies.rs",
                "crates/runtime/tests/tasks.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/codex_accounts/pool_scheduling.rs",
            required_current_files: &["crates/core/tests/account_pool.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/codex_accounts/refresh.rs",
            required_current_files: &[
                "crates/core/tests/protocol.rs",
                "crates/runtime/tests/token_refresh.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/codex_accounts/refresh_scheduler.rs",
            required_current_files: &["crates/core/tests/account_pool.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/codex_accounts/repository.rs",
            required_current_files: &[
                "crates/adapters/tests/account_repository.rs",
                "crates/adapters/tests/refresh_leases.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/codex_accounts/service_refresh.rs",
            required_current_files: &["crates/runtime/tests/token_refresh.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/codex_events/logs_pagination.rs",
            required_current_files: &[
                "crates/adapters/tests/events.rs",
                "crates/server/tests/admin_logs_routes.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/codex_gateway/cli_auth_import.rs",
            required_current_files: &["crates/server/tests/admin_accounts_routes.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/codex_gateway/fingerprint_update.rs",
            required_current_files: &[
                "crates/adapters/tests/codex.rs",
                "crates/runtime/tests/fingerprint.rs",
                "crates/runtime/tests/tasks.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/codex_gateway/headers.rs",
            required_current_files: &[
                "crates/adapters/tests/codex.rs",
                "crates/core/tests/protocol.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/codex_gateway/http_client.rs",
            required_current_files: &["crates/adapters/tests/codex.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/codex_gateway/oauth_refresh.rs",
            required_current_files: &["crates/adapters/tests/oauth.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/codex_gateway/usage_events.rs",
            required_current_files: &["crates/core/tests/protocol.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/codex_gateway/websocket.rs",
            required_current_files: &[
                "crates/core/tests/protocol.rs",
                "crates/adapters/tests/codex.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/codex_gateway/websocket/pool.rs",
            required_current_files: &[
                "crates/adapters/tests/codex.rs",
                "crates/runtime/tests/upstream.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/codex_models/catalog.rs",
            required_current_files: &[
                "crates/core/tests/models.rs",
                "crates/adapters/tests/models.rs",
                "crates/server/tests/admin_models_routes.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/codex_serving/chat_completions.rs",
            required_current_files: &[
                "crates/server/tests/openai_chat_upstream.rs",
                "crates/core/tests/protocol.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/codex_serving/diagnostics_route.rs",
            required_current_files: &[
                "crates/server/tests/openai_diagnostics_routes.rs",
                "crates/server/tests/admin_settings_routes.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/codex_serving/responses_http_sse.rs",
            required_current_files: &["crates/server/tests/openai_chat_upstream.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/codex_serving/responses_websocket.rs",
            required_current_files: &[
                "crates/server/tests/openai_chat_upstream.rs",
                "crates/adapters/tests/codex.rs",
                "crates/runtime/tests/upstream.rs",
                "crates/runtime/tests/session_affinity.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/codex_serving/routes_chat.rs",
            required_current_files: &["crates/core/tests/protocol.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/codex_serving/routes_responses.rs",
            required_current_files: &[
                "crates/server/tests/openai_responses_routes.rs",
                "crates/server/tests/openai_models_auth.rs",
                "crates/core/tests/models.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/codex_serving/upstream_errors.rs",
            required_current_files: &["crates/server/tests/openai_chat_upstream.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/codex_serving/upstream_fallback.rs",
            required_current_files: &["crates/server/tests/openai_chat_upstream.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/config.rs",
            required_current_files: &["crates/platform/tests/config.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/platform/client_key_auth.rs",
            required_current_files: &["crates/platform/tests/client_key_auth.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/platform/crypto.rs",
            required_current_files: &["crates/platform/tests/crypto.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/platform/http_auth.rs",
            required_current_files: &[
                "crates/platform/tests/client_key_auth.rs",
                "crates/server/tests/openai_models_auth.rs",
                "crates/server/tests/admin_session_routes.rs",
            ],
        },
        BaselineSuite {
            baseline_path: "tests/platform/log_rotation.rs",
            required_current_files: &["crates/platform/tests/log_rotation.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/platform/storage_schema.rs",
            required_current_files: &["crates/platform/tests/storage_schema.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/runtime/http_trace.rs",
            required_current_files: &["crates/server/tests/http_trace_middleware.rs"],
        },
        BaselineSuite {
            baseline_path: "tests/runtime/startup.rs",
            required_current_files: &[
                "crates/runtime/tests/account_pool_restore.rs",
                "crates/runtime/tests/session_affinity.rs",
                "crates/server/tests/openai_chat_upstream.rs",
            ],
        },
    ]
}
