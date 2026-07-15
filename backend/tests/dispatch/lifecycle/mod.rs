mod stream;

use std::{fs, path::Path};

#[test]
fn backend_src_should_not_contain_test_attributes() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert_no_test_attributes(&source_root);
}

#[test]
fn live_responses_sse_should_have_one_canonical_decoder_owner() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let canonical = source(manifest, "src/dispatch/transport/canonical.rs");
    assert!(
        canonical.contains("parse_sse_events"),
        "canonical transport must own SSE decoding"
    );

    for relative in [
        "src/api/client/responses/websocket.rs",
        "src/dispatch/stream/live.rs",
        "src/dispatch/failure/sse.rs",
        "src/dispatch/controllers/telemetry/events.rs",
    ] {
        let source = source(manifest, relative);
        for forbidden in [
            "SseEventDecoder",
            "parse_sse_events",
            "response_from_codex_sse",
        ] {
            assert!(
                !source.contains(forbidden),
                "{relative} must consume canonical facts instead of {forbidden}"
            );
        }
    }
}

#[test]
fn attempt_retry_contract_should_have_one_pipeline_owner() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let pipeline = source(manifest, "src/dispatch/lifecycle/pipeline.rs");
    for required in [
        "runner.next().await?",
        ".handle_attempt(attempt.controller_scope()?, attempt.observation())",
        "attempt.apply(decision).await?",
        "AttemptStep::Committed(attempt)",
    ] {
        assert!(
            pipeline.contains(required),
            "attempt pipeline must own {required}"
        );
    }
    assert_eq!(
        pipeline.matches(".handle_attempt(").count(),
        1,
        "each attempt observation must enter the controller composition exactly once"
    );
    for forbidden in [
        "observe_attempt(",
        "controllers.decide(",
        "controllers.last_failure(",
        "ResponseDispatchError::NoActiveAccount",
    ] {
        assert!(
            !pipeline.contains(forbidden),
            "attempt pipeline must not split classification across {forbidden}"
        );
    }

    for relative in [
        "src/dispatch/service.rs",
        "src/dispatch/stream/lifecycle.rs",
    ] {
        let adapter = source(manifest, relative);
        for forbidden in [
            "runner.next()",
            ".handle_attempt(",
            "observe_attempt(",
            "controllers.decide(",
            "runner.apply(",
            "ResponseDispatchError::NoActiveAccount",
        ] {
            assert!(
                !adapter.contains(forbidden),
                "{relative} must map terminal results instead of owning {forbidden}"
            );
        }
    }
}

#[test]
fn lifecycle_contract_should_not_leak_feature_owners() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        "src/dispatch/lifecycle/attempt.rs",
        "src/dispatch/lifecycle/contract.rs",
        "src/dispatch/lifecycle/pipeline.rs",
        "src/dispatch/lifecycle/request.rs",
    ] {
        let lifecycle = source(manifest, relative);
        for forbidden in [
            "HistoryRecoveryPlan",
            "HistorySource",
            "QuotaLimitReached",
            "quota_limit_reached",
        ] {
            assert!(
                !lifecycle.contains(forbidden),
                "{relative} must expose generic lifecycle facts instead of {forbidden}"
            );
        }
    }
}

#[test]
fn stream_commit_contract_should_decode_once_before_establishment() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let attempt = source(manifest, "src/dispatch/lifecycle/attempt.rs");
    let contract = source(manifest, "src/dispatch/lifecycle/contract.rs");
    let pipeline = source(manifest, "src/dispatch/lifecycle/pipeline.rs");
    let prefetch = source(manifest, "src/dispatch/transport/prefetch.rs");

    assert!(attempt.contains("attempt_request.stream_commit_policy"));
    assert!(!attempt.contains("decoder.push(prefetched"));
    assert!(prefetch.contains("while !decoder.commit_boundary_reached(policy)"));
    assert!(prefetch.contains("let batch = decoder.push(chunk)?"));
    assert!(prefetch.contains("initial_batch.append(batch)"));

    let open = between(&attempt, "impl OpenAttempt<'_, '_> {", "/// 已越过提交边界");
    assert!(open.contains("decision: AttemptDecision"));
    let committed = between(
        &attempt,
        "impl CommittedAttempt {",
        "/// `next` 产出的提交 typestate",
    );
    assert!(committed.contains("fn accept(self)"));
    assert!(!committed.contains("AttemptDecision"));
    assert!(!committed.contains("Retry"));

    for obsolete in [
        "AttemptCommitState",
        "RetryAfterCommit",
        "validate_attempt_decision",
        "StreamResponseFacts::Ready",
    ] {
        assert!(!attempt.contains(obsolete));
        assert!(!contract.contains(obsolete));
        assert!(!pipeline.contains(obsolete));
    }
}

#[test]
fn live_stream_exit_paths_should_share_one_consuming_finalizer() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let live = source(manifest, "src/dispatch/stream/live.rs");
    let finalizer = source(manifest, "src/dispatch/lifecycle/finalizer.rs");

    assert_eq!(
        live.matches(".finalize(").count(),
        1,
        "the live stream may invoke exactly one finalizer"
    );
    assert!(
        finalizer.contains("async fn finalize(\n        self,"),
        "the finalizer must consume itself so it cannot run twice"
    );
    for terminal_path in [
        "break StreamTerminal::Cancelled",
        "break StreamTerminal::Shutdown",
        "return Err(StreamTerminal::CaptureLimitExceeded)",
        "return Err(StreamTerminal::DownstreamClosed)",
    ] {
        assert!(
            live.contains(terminal_path),
            "{terminal_path} must unwind through the shared finalizer"
        );
    }
    let leave = finalizer
        .find(".leave_stream(")
        .expect("controllers should leave from the finalizer");
    let lease = finalizer
        .find("let mut lease_completion = Box::pin(account_lease.complete())")
        .expect("the account lease should finalize");
    assert!(finalizer.contains("timeout(LEASE_FINALIZE_TIMEOUT, lease_completion.as_mut())"));
    assert!(finalizer.contains("tokio::spawn(lease_completion)"));
    let client = finalizer
        .find("finish_client_stream(")
        .expect("client termination should run after internal finalization");
    assert!(leave < lease && lease < client);
}

#[test]
fn controller_exit_order_should_be_static_and_effect_only() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let controllers = source(manifest, "src/dispatch/controllers/mod.rs");
    let pipeline = source(manifest, "src/dispatch/lifecycle/pipeline.rs");
    let complete = source(manifest, "src/dispatch/service.rs");
    let stream = source(manifest, "src/dispatch/stream/lifecycle.rs");

    for forbidden in [
        "EnteredControllerScopes",
        "RequestControllerStage",
        "AttemptControllerStage",
        "ControllerScopeError",
        "unwind_attempt",
        "finalize_request",
        "leave_next()",
        "into_reverse()",
    ] {
        assert!(
            !controllers.contains(forbidden),
            "controller composition must not retain runtime marker path {forbidden}"
        );
    }
    assert!(!pipeline.contains("unwind_attempt"));
    assert!(!complete.contains("finalize_request"));
    assert!(!stream.contains("finalize_request"));
    assert_eq!(complete.matches(".finalize_complete(").count(), 1);

    let complete_exit = between(
        &controllers,
        "async fn finalize_complete(",
        "pub(in crate::dispatch) fn new(",
    );
    assert_appears_in_order(
        complete_exit,
        &[
            "CloudflareController::leave_complete",
            "UsageController::leave_complete",
            "AffinityController::leave_complete",
            "TelemetryController::leave_complete",
        ],
    );

    let stream_exit = between(
        &controllers,
        "async fn leave_stream(",
        "pub(in crate::dispatch) async fn handle_attempt(",
    );
    assert_appears_in_order(
        stream_exit,
        &[
            "self.apply_stream_account_state",
            "CloudflareController::leave_stream",
            "UsageController::leave_stream",
            "AffinityController::leave_stream",
            "TelemetryController::leave_stream",
            "self.apply_stream_policy_failure",
        ],
    );
}

fn source(manifest: &Path, relative: &str) -> String {
    fs::read_to_string(manifest.join(relative)).expect("Rust source should be readable")
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

fn assert_no_test_attributes(path: &Path) {
    let entries = fs::read_dir(path).expect("backend/src should be readable");
    for entry in entries {
        let entry = entry.expect("backend/src entries should be readable");
        let path = entry.path();
        if path.is_dir() {
            assert_no_test_attributes(&path);
            continue;
        }
        if path.extension().is_some_and(|extension| extension == "rs") {
            let source = fs::read_to_string(&path).expect("Rust source should be readable");
            for marker in ["#[cfg(test)]", "#[test]", "#[tokio::test]"] {
                assert!(
                    !source.contains(marker),
                    "test marker {marker:?} must stay outside backend/src: {}",
                    path.display()
                );
            }
        }
    }
}
