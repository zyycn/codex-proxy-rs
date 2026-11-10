//! 验证 Messages 缓存用量合并与统一输入量

use gateway_protocol::anthropic::{MessageUsage, messages_to_responses_response};
use gateway_protocol::openai::events::{billable_usage_is_complete, extract_usage};
use serde_json::{Map, json};

#[test]
fn usage_deltas_replace_counters_and_keep_cache_facts() {
    let mut usage = MessageUsage::from_value(
        &json!({"input_tokens":4,"output_tokens":1,"cache_read_input_tokens":20,"cache_creation_input_tokens":10}),
    );
    usage.merge(&json!({"output_tokens":9}));
    assert_eq!(usage.total_input_tokens(), Some(34));
    assert_eq!(usage.output_tokens, Some(9));
}

#[test]
fn converted_usage_matches_canonical_cache_write_accounting() {
    let converted=messages_to_responses_response(&json!({"id":"m1","content":[],"stop_reason":"end_turn","usage":{"input_tokens":4,"output_tokens":9,"cache_read_input_tokens":20,"cache_creation_input_tokens":10}}),&Map::new()).expect("用量转换");
    assert!(converted["usage"].get("output_tokens_details").is_none());
    let usage = extract_usage(&converted).expect("提取统一用量");
    assert_eq!(usage.input_tokens, 34);
    assert_eq!(usage.cache_write_input_tokens, 10);
    assert_eq!(usage.cached_input_tokens, 20);
    assert_eq!(usage.total_tokens, 43);
    assert!(billable_usage_is_complete(&converted, usage));
}
