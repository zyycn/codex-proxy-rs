//! 验证账号选择诊断保留选择依据，并限制候选记录数量

use gateway_core::account::{AccountSelector, RotationStrategy};
use gateway_core::diagnostics::TraceContext;
use serde_json::json;

use super::{candidate, candidate_with_concurrency, context};

#[test]
fn selection_trace_should_distinguish_load_quota_and_affinity() {
    let mut candidates = [
        candidate_with_concurrency("acct_74", 1, 10),
        candidate_with_concurrency("acct_96", 0, 10),
    ];
    candidates[0].signals.quota_remaining_rank = Some(2600);
    candidates[1].signals.quota_remaining_rank = Some(400);
    let mut context = context(RotationStrategy::Smart);
    let trace = TraceContext::new("req_smart_trace");
    let selection = AccountSelector.select(&candidates, &context);
    trace.account_selection(&candidates, &context, selection.as_ref());
    context.preferred_account = Some(candidates[1].account.id().clone());
    let selection = AccountSelector.select(&candidates, &context);
    trace.account_selection(&candidates, &context, selection.as_ref());

    let snapshot = trace.snapshot().expect("trace");
    let first = &snapshot["events"][0]["data"];
    let second = &snapshot["events"][1]["data"];
    assert_eq!(first["selectedAccountId"], "acct_74");
    assert_eq!(first["preferredResult"], "NotRequested");
    let busy = &first["candidates"][0];
    assert_eq!(busy["inFlight"], 1);
    assert_eq!(busy["concurrencyLimit"], 10);
    assert_eq!(busy["quotaRemainingBasisPoints"], 2600);
    assert!(busy["blocker"].is_null());
    assert!(
        busy["smartScore"].as_f64().expect("score")
            > first["candidates"][1]["smartScore"]
                .as_f64()
                .expect("score")
    );
    assert_eq!(second["selectedAccountId"], "acct_96");
    assert_eq!(second["preferredResult"], "Hit");
}

#[test]
fn selection_trace_should_bound_candidates_and_preserve_the_selected_account() {
    let mut candidates = (0..30)
        .map(|index| candidate(&format!("acct_{index}"), 0, None))
        .collect::<Vec<_>>();
    candidates.push(candidate("acct_best", 0, Some(10_000)));
    let trace = TraceContext::new("req_many_candidates");
    // 选号位于首部保留区之外，也应在长流的常规事件淘汰后保留
    for _ in 0..10 {
        trace.record("test.event", json!({}));
    }
    let context = context(RotationStrategy::Smart);
    let selection = AccountSelector.select(&candidates, &context);
    trace.account_selection(&candidates, &context, selection.as_ref());
    for index in 0..400 {
        trace.record("test.event", json!({"index": index}));
    }

    let snapshot = trace.snapshot().expect("trace");
    let event = snapshot["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| event["stage"] == "account.selection")
        .expect("selection survives event eviction");
    let data = &event["data"];
    let recorded = data["candidates"].as_array().expect("bounded candidates");
    assert!(recorded.len() < candidates.len());
    assert_eq!(recorded[0]["accountId"], "acct_best");
    assert!(recorded[1]["quotaRemainingBasisPoints"].is_null());
    assert_eq!(
        recorded.len() as u64 + data["omittedCandidates"].as_u64().expect("omitted"),
        candidates.len() as u64
    );
    assert!(data.to_string().len() < 4096);
}

#[test]
fn selection_trace_should_explain_when_all_accounts_are_busy() {
    let candidates = [candidate_with_concurrency("acct_busy", 10, 10)];
    let trace = TraceContext::new("req_no_candidate");
    let context = context(RotationStrategy::Smart);
    let selection = AccountSelector.select(&candidates, &context);
    assert!(selection.is_none());
    trace.account_selection(&candidates, &context, selection.as_ref());
    let snapshot = trace.snapshot().expect("trace");
    let data = &snapshot["events"][0]["data"];
    assert!(data["selectedAccountId"].is_null());
    assert_eq!(data["candidates"][0]["blocker"], "ConcurrencyLimit");
}
