//! 验证插件事实查询兼容新增响应字段并拒绝无效已知值

use gateway_plugin_sdk::call::data::{
    AccountFactsPage, AccountFactsQuery, ClientKeyFacts, ClientKeyFactsQuery, QuotaFacts,
    QuotaFactsQuery,
};
use serde_json::{Value, json};

fn account_page() -> Value {
    json!({
        "schema_version":1,
        "accounts":[{
            "account_id":"acct_1", "provider_id":"openai", "name":"测试账号",
            "email":null, "group_ids":[], "enabled":true, "updated_at_ms":0
        }],
        "next_cursor":null
    })
}

#[test]
fn account_facts_accept_additive_page_and_account_fields() {
    let original = account_page();
    let mut extended = original.clone();
    extended["future_page_field"] = json!({"value":1});
    extended["accounts"][0]["future_account_field"] = json!(["value"]);
    let page: AccountFactsPage = serde_json::from_value(extended).unwrap();
    assert_eq!(serde_json::to_value(page).unwrap(), original);
}

#[test]
fn key_facts_accept_additive_fields() {
    let original = json!({
        "schema_version":1, "client_key_id":"key_1", "enabled":false,
        "group_ids":["grp_1"]
    });
    let mut extended = original.clone();
    extended["future_key_field"] = json!({"value":1});
    let facts: ClientKeyFacts = serde_json::from_value(extended).unwrap();
    assert_eq!(serde_json::to_value(facts).unwrap(), original);
}

#[test]
fn quota_facts_accept_additive_result_and_window_fields() {
    let original = json!({
        "schema_version":1, "account_id":"acct_1", "observed_at_ms":null,
        "windows":[{
            "key":"weekly", "window_seconds":604800,
            "used_percent":null, "reset_at_ms":null
        }]
    });
    let mut extended = original.clone();
    extended["future_quota_field"] = json!({"value":1});
    extended["windows"][0]["future_window_field"] = json!(["value"]);
    let facts: QuotaFacts = serde_json::from_value(extended).unwrap();
    assert_eq!(serde_json::to_value(facts).unwrap(), original);
}

#[test]
fn account_facts_still_require_known_fields_and_valid_types() {
    let mut missing_name = account_page();
    missing_name["accounts"][0]
        .as_object_mut()
        .unwrap()
        .remove("name");
    assert!(serde_json::from_value::<AccountFactsPage>(missing_name).is_err());

    for (field, invalid) in [
        ("name", json!(42)),
        ("email", json!(false)),
        ("enabled", json!("true")),
    ] {
        let mut page = account_page();
        page["accounts"][0][field] = invalid;
        assert!(
            serde_json::from_value::<AccountFactsPage>(page).is_err(),
            "{field}"
        );
    }
}

#[test]
fn facts_queries_still_reject_unknown_fields() {
    assert!(serde_json::from_value::<AccountFactsQuery>(json!({"limit":1,"limti":2})).is_err());
    assert!(
        serde_json::from_value::<ClientKeyFactsQuery>(
            json!({"client_key_id":"key_1","instance_id":"forged"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<QuotaFactsQuery>(json!({"account_id":"acct_1","refresh":true}))
            .is_err()
    );
}
