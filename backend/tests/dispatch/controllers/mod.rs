use std::{fs, path::Path};

const CONTROLLER_OWNERS: &[(&str, &str)] = &[
    ("account_failure.rs", "AccountFailureController"),
    ("affinity.rs", "AffinityController"),
    ("cloudflare.rs", "CloudflareController"),
    ("cyber_policy/mod.rs", "CyberPolicyController"),
    ("history.rs", "HistoryController"),
    ("quota.rs", "QuotaController"),
    ("telemetry/mod.rs", "TelemetryController"),
    ("usage.rs", "UsageController"),
];

#[test]
fn controller_owners_must_not_call_each_other() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/dispatch/controllers");

    for (relative, own_controller) in CONTROLLER_OWNERS {
        let source = read(&root.join(relative));
        for (_, sibling_controller) in CONTROLLER_OWNERS {
            if sibling_controller == own_controller {
                continue;
            }
            assert!(
                !source.contains(sibling_controller),
                "controller owner {relative} must not call sibling {sibling_controller}; compose them in ControllerSet"
            );
        }
    }
}

#[test]
fn cyber_policy_semantics_must_stay_inside_its_owner() {
    let dispatch = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/dispatch");
    let mut violations = Vec::new();
    collect_cyber_policy_violations(&dispatch, &dispatch, &mut violations);

    assert!(
        violations.is_empty(),
        "cyber_policy feature semantics escaped its controller owner: {}",
        violations.join(", ")
    );
}

#[test]
fn cyber_policy_owner_should_construct_its_own_attempt_decision() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let composition = read(&manifest.join("src/dispatch/controllers/mod.rs"));
    let cyber_policy = read(&manifest.join("src/dispatch/controllers/cyber_policy/mod.rs"));
    let decision = between(
        &composition,
        "fn attempt_decision(",
        "fn default_attempt_decision(",
    );

    assert!(cyber_policy.contains("pub(super) fn decision("));
    assert!(cyber_policy.contains("AttemptDecision::RetryNextCandidate"));
    assert!(decision.contains("CyberPolicyController::decision(observation, classified)"));
    assert!(!decision.contains("can_retry_next_candidate"));
    assert!(!decision.contains("AttemptDecision::RetryNextCandidate"));
}

#[test]
fn attempt_and_stream_should_share_one_feature_failure_owner_order() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let composition = read(&manifest.join("src/dispatch/controllers/mod.rs"));
    let shared = between(
        &composition,
        "fn classify_shared_failure",
        "fn attempt_decision(",
    );

    assert_eq!(composition.matches("fn classify_shared_failure").count(), 1);
    assert!(
        composition
            .contains("Self::classify_shared_failure(FailureObservation::Attempt(observation))")
    );
    assert!(composition.contains("Self::classify_shared_failure(FailureObservation::Stream {"));
    assert_appears_in_order(
        shared,
        &[
            "fact.and_then(CyberPolicyController::classify)",
            "AccountFailureController::classify(observation)",
            "QuotaController::classify(observation)",
        ],
    );
    assert!(!composition.contains("self.cyber_policy.owns_failure"));
}

#[test]
fn request_enter_io_should_be_parallel_and_bounded() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/dispatch/controllers");
    let composition = read(&root.join("mod.rs"));
    let history = read(&root.join("history.rs"));
    let affinity = read(&root.join("affinity.rs"));

    assert!(composition.contains("tokio::join!("));
    for operation in [
        "HistoryController::enter(",
        "self.cyber_policy.prepare(request)",
        "AffinityController::preferred_account_id(",
    ] {
        assert!(
            composition.contains(operation),
            "request enter must run {operation} in its parallel I/O group"
        );
    }
    for (owner, source) in [("history", history), ("affinity", affinity)] {
        assert!(source.contains("Duration::from_millis(100)"));
        assert!(
            source.contains("timeout("),
            "{owner} Redis I/O must be best-effort bounded"
        );
    }
}

#[test]
fn controller_best_effort_io_should_share_one_bounded_owner() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let composition = read(&manifest.join("src/dispatch/controllers/mod.rs"));
    let finalizer = read(&manifest.join("src/dispatch/lifecycle/finalizer.rs"));

    assert!(
        composition
            .contains("const BEST_EFFORT_IO_TIMEOUT: Duration = Duration::from_millis(100);")
    );
    assert!(composition.contains("match timeout(BEST_EFFORT_IO_TIMEOUT, operation).await"));
    for operation in [
        "\"cookie.complete\"",
        "\"quota.complete_headers\"",
        "\"cookie.attempt_error\"",
        "\"telemetry.attempt_error\"",
        "\"cloudflare.complete\"",
        "\"usage.complete\"",
        "\"telemetry.complete\"",
        "\"affinity.complete\"",
        "\"cloudflare\"",
        "\"usage\"",
        "\"telemetry\"",
    ] {
        assert!(
            composition.contains(operation),
            "controller I/O {operation} must use the shared best-effort timeout"
        );
    }
    assert!(finalizer.contains("timeout(LEASE_FINALIZE_TIMEOUT, account_lease.complete())"));
}

#[test]
fn typed_client_failure_should_reach_api_without_reclassification() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let composition = read(&manifest.join("src/dispatch/controllers/mod.rs"));
    let errors = read(&manifest.join("src/dispatch/errors.rs"));
    let api = read(&manifest.join("src/api/client/errors.rs"));

    assert!(composition.contains("fn default_response_client_failure("));
    assert!(errors.contains("pub struct ClientFailure"));
    assert!(api.contains("client_failure.status_code()"));
    assert!(api.contains("client_failure.exposed_upstream()?"));
    for forbidden in [
        "ControllerSet",
        "classify_client_failure",
        "client_failure_http_status",
    ] {
        assert!(!errors.contains(forbidden));
        assert!(!api.contains(forbidden));
    }
}

#[test]
fn telemetry_exits_should_only_be_mounted_through_controller_set() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        "src/dispatch/service.rs",
        "src/dispatch/stream/lifecycle.rs",
    ] {
        let adapter = read(&manifest.join(relative));
        for forbidden in [
            "controllers::telemetry",
            "record_response_dispatch_error_event",
            "record_prefetched_response_stream_failure_event",
        ] {
            assert!(
                !adapter.contains(forbidden),
                "{relative} must call a typed ControllerSet hook instead of {forbidden}"
            );
        }
    }
}

#[test]
fn history_policy_should_not_escape_to_generic_routing() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let routing = manifest.join("src/dispatch/routing");
    assert!(!routing.join("history.rs").exists());
    let candidates = read(&routing.join("candidates.rs"));
    for forbidden in ["previous_response", "History", "replay"] {
        assert!(
            !candidates.contains(forbidden),
            "generic candidate routing must not own {forbidden} policy"
        );
    }
}

#[test]
fn canonical_failure_parser_should_extract_facts_without_business_code_mapping() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let parser = read(&manifest.join("src/upstream/openai/protocol/responses.rs"));
    for raw_fact in [
        "failure_type",
        "failure_explicit_status_code",
        "retry_after_seconds_from_value",
    ] {
        assert!(
            parser.contains(raw_fact),
            "canonical failure parser must preserve {raw_fact}"
        );
    }
    for forbidden in [
        "usage_limit_reached",
        "quota_exhausted",
        "previous_response_not_found",
        "account_banned",
    ] {
        assert!(
            !parser.contains(forbidden),
            "protocol parser must not classify business code {forbidden}"
        );
    }
}

#[test]
fn telemetry_business_logic_should_live_under_controller_owner() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(!manifest.join("src/dispatch/recording.rs").exists());
    assert!(
        manifest
            .join("src/dispatch/controllers/telemetry/events.rs")
            .exists()
    );
}

#[test]
fn controllers_must_not_depend_on_live_stream_executor() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let controllers = manifest.join("src/dispatch/controllers");
    let mut violations = Vec::new();
    collect_live_executor_dependencies(&controllers, &controllers, &mut violations);

    assert!(
        violations.is_empty(),
        "controllers must consume typed exits instead of live executor context: {}",
        violations.join(", ")
    );

    let live = read(&manifest.join("src/dispatch/stream/live.rs"));
    for forbidden in [
        "LiveResponseStreamContext",
        "ControllerSet",
        "SessionAffinityService",
        "AccountPoolService",
        "CloudflareRecovery",
        "Recorder",
    ] {
        assert!(
            !live.contains(forbidden),
            "live stream executor must only hold transport lifecycle state, not {forbidden}"
        );
    }
    assert!(live.contains("finalizer: StreamFinalizer"));
}

fn collect_cyber_policy_violations(root: &Path, path: &Path, violations: &mut Vec<String>) {
    for entry in fs::read_dir(path).expect("dispatch source directory should be readable") {
        let entry = entry.expect("dispatch source entry should be readable");
        let path = entry.path();
        if path.is_dir() {
            collect_cyber_policy_violations(root, &path, violations);
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let relative = path.strip_prefix(root).expect("dispatch source path");
        if relative == Path::new("controllers/mod.rs")
            || relative.starts_with("controllers/cyber_policy")
        {
            continue;
        }
        let source = read(&path);
        if source.contains("cyber_policy") || source.contains("CyberPolicy") {
            violations.push(relative.display().to_string());
        }
    }
}

fn collect_live_executor_dependencies(root: &Path, path: &Path, violations: &mut Vec<String>) {
    for entry in fs::read_dir(path).expect("controller source directory should be readable") {
        let entry = entry.expect("controller source entry should be readable");
        let path = entry.path();
        if path.is_dir() {
            collect_live_executor_dependencies(root, &path, violations);
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let source = read(&path);
        if source.contains("stream::live") || source.contains("LiveResponseStreamContext") {
            violations.push(
                path.strip_prefix(root)
                    .expect("controller source path")
                    .display()
                    .to_string(),
            );
        }
    }
}

fn between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source
        .find(start)
        .expect("source start marker should exist");
    let tail = &source[start..];
    let end = tail.find(end).expect("source end marker should exist");
    &tail[..end]
}

fn assert_appears_in_order(source: &str, required: &[&str]) {
    let mut cursor = 0;
    for marker in required {
        let offset = source[cursor..]
            .find(marker)
            .unwrap_or_else(|| panic!("ordered source marker should exist: {marker}"));
        cursor += offset + marker.len();
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}
