use codex_proxy_rs::codex::serving::{
    chat::ChatService, diagnostics::DiagnosticsService, dispatch::affinity::SessionAffinityMap,
    http::router::router, responses::ResponsesService,
};
use std::{fs, path::PathBuf};

#[test]
fn serving_exports_client_facing_proxy_modules() {
    let _chat_type = std::any::type_name::<ChatService>();
    let _responses_type = std::any::type_name::<ResponsesService>();
    let _diagnostics_type = std::any::type_name::<DiagnosticsService>();
    let _affinity_type = std::any::type_name::<SessionAffinityMap>();
    let _router_fn = router;
}

#[test]
fn serving_dispatch_has_dedicated_recovery_transition_boundary() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let transition_path = manifest_dir.join("src/codex/serving/dispatch/transition.rs");
    let transition = fs::read_to_string(&transition_path).unwrap_or_else(|error| {
        panic!(
            "expected recovery transition boundary at {}: {error}",
            transition_path.display()
        )
    });
    let dispatch_mod =
        fs::read_to_string(manifest_dir.join("src/codex/serving/dispatch/mod.rs")).unwrap();
    let fallback =
        fs::read_to_string(manifest_dir.join("src/codex/serving/dispatch/fallback.rs")).unwrap();

    assert!(
        dispatch_mod.contains("mod transition;"),
        "dispatch should declare a transition module"
    );
    assert!(
        transition.contains("execute_upstream_account_recovery_transition_with_deps"),
        "account recovery side effects should live in the transition boundary"
    );
    assert!(
        transition.contains("execute_upstream_request_recovery_transition_with_deps"),
        "request recovery side effects should live in the transition boundary"
    );
    assert!(
        !fallback.contains("acquire_with("),
        "fallback classification should not acquire fallback accounts directly"
    );
}
