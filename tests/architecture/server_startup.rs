use std::{fs, path::Path};

#[test]
fn server_entrypoint_should_own_background_task_lifecycle() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let main_rs = root.join("crates/server/src/main.rs");
    let content = fs::read_to_string(&main_rs).expect("server main should be readable");

    assert!(
        content.contains("start_background_tasks(&state).await")
            && content.contains("task_coordinator.shutdown().await"),
        "server main must start runtime background tasks and shut them down after axum exits"
    );
}

#[test]
fn server_entrypoint_should_inject_runtime_installation_id() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let main_rs = root.join("crates/server/src/main.rs");
    let content = fs::read_to_string(&main_rs).expect("server main should be readable");

    assert!(
        content.contains("load_or_create_installation_id(Some(&data_dir))")
            && content.contains("with_pool_secret_api_key_hasher_and_installation_id"),
        "server main must load the platform installation id and inject it into runtime state"
    );
}

#[test]
fn server_entrypoint_should_restore_session_affinity_before_serving() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let main_rs = root.join("crates/server/src/main.rs");
    let content = fs::read_to_string(&main_rs).expect("server main should be readable");

    assert!(
        content.contains("restore_session_affinity_from_repository_now().await"),
        "server main must restore session affinity mappings from SQLite during startup"
    );
}

#[test]
fn server_entrypoint_should_restore_account_pool_before_serving() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let main_rs = root.join("crates/server/src/main.rs");
    let content = fs::read_to_string(&main_rs).expect("server main should be readable");

    assert!(
        content.contains("restore_account_pool_from_repository().await"),
        "server main must restore active runtime accounts from SQLite during startup"
    );
}

#[test]
fn server_entrypoint_should_ensure_default_admin_before_serving() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let main_rs = root.join("crates/server/src/main.rs");
    let content = fs::read_to_string(&main_rs).expect("server main should be readable");

    assert!(
        content.contains("ensure_default_admin")
            && content.contains("config.admin.default_password"),
        "server main must initialize the configured default admin before serving admin routes"
    );
}
