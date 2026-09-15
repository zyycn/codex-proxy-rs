use gateway_core::account::AccountModelAccess;
use serde_json::json;

#[test]
fn account_model_access_document_rejects_invalid_or_unbounded_settings() {
    for invalid in [
        json!({"mode":"invalid","models":[]}),
        json!({"mode":"all","models":["test-model"]}),
        json!({"mode":"allowlist","models":[]}),
        json!({"mode":"denylist","models":[]}),
        json!({"mode":"allowlist","models":["gpt-*"]}),
        json!({"mode":"allowlist","models":[" test-model"]}),
        json!({"mode":"allowlist","models":["\n"]}),
        json!({"mode":"allowlist","models":["__internal"]}),
        json!({"mode":"allowlist","models":["x".repeat(257)]}),
        json!({"mode":"allowlist","models":vec!["test-model";257]}),
        json!({"mode":"all","models":[],"unknown":true}),
        json!({"mode":"all"}),
    ] {
        assert!(serde_json::from_value::<AccountModelAccess>(invalid).is_err());
    }
}

#[test]
fn account_model_access_document_deduplicates_without_changing_exact_ids() {
    let policy: AccountModelAccess = serde_json::from_value(json!({
        "mode":"allowlist","models":["test-luna","test-luna","Test-Luna"]
    }))
    .expect("policy");
    assert_eq!(
        serde_json::to_value(policy).expect("serialize"),
        json!({
            "mode":"allowlist","models":["Test-Luna","test-luna"]
        })
    );
}
