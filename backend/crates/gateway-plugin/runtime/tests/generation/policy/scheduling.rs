//! 验证真实插件进程的候选过滤、繁忙投影与非法决定边界

use super::*;
use gateway_core::engine::policy::AccountPolicyError;

#[tokio::test]
async fn scheduler_filter_contract_covers_busy_candidates_and_rejects_out_of_scope_ids() {
    for (decision, expected) in [
        (
            serde_json::json!({"decision":"delegate_filtered","exclude_account_ids":["acct_high", "acct_high"]}),
            "acct_low",
        ),
        (
            serde_json::json!({"decision":"delegate_filtered","exclude_account_ids":["acct_high", "acct_low"]}),
            "filtered",
        ),
        (
            serde_json::json!({"decision":"delegate_filtered","exclude_account_ids":["acct_unknown"]}),
            "fault",
        ),
        (
            serde_json::json!({"decision":"pick","account_id":"acct_high"}),
            "fault",
        ),
    ] {
        let marker = tempfile::NamedTempFile::new().unwrap();
        let (cache, runtime) = setup(vec![InstanceFixture {
            id: "scheduler",
            configuration: serde_json::json!({"schedule_decision":decision, "schedule_marker": marker.path()}),
            bindings: vec![binding(SCHEDULER_CONTRIBUTION, "scheduling", 0, PluginFailurePolicy::Reject)],
        }]).await;
        let generation = prepare(&runtime).await;
        let policy = policy_context(&runtime, generation.clone(), "req_filter");
        let candidates = [candidate("acct_high", 100), candidate("acct_low", 10)];
        let mut selection = selection_context();
        selection
            .busy_accounts
            .insert(candidates[0].account.id().clone());
        selection.preferred_account = Some(candidates[0].account.id().clone());
        let result = policy
            .select_account(
                NonZeroU32::MIN,
                &ProviderKind::new("example").unwrap(),
                None,
                &candidates,
                &mut selection,
            )
            .await;
        match expected {
            "filtered" => assert!(matches!(result, Err(AccountPolicyError::Filtered))),
            "fault" => assert!(matches!(result, Err(AccountPolicyError::Fault))),
            id => assert_eq!(
                result.unwrap().unwrap().candidate().account.id().as_str(),
                id
            ),
        }
        let request: serde_json::Value =
            serde_json::from_str(std::fs::read_to_string(marker.path()).unwrap().trim()).unwrap();
        assert_eq!(
            request["request"]["candidates"].as_array().unwrap().len(),
            2
        );
        assert_eq!(request["request"]["candidates"][0]["available"], false);
        assert_eq!(request["request"]["candidates"][1]["available"], true);
        drop(policy);
        drop(generation);
        super::super::wait_until_empty(cache.path()).await;
    }
}
