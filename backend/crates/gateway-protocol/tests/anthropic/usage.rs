//! 验证 Messages 缓存用量合并与统一输入量

use gateway_protocol::anthropic::MessageUsage;
use serde_json::json;

#[test]
fn usage_deltas_replace_counters_and_keep_cache_facts() {
    let mut usage = MessageUsage::from_value(
        &json!({"input_tokens":4,"output_tokens":1,"cache_read_input_tokens":20,"cache_creation_input_tokens":10}),
    );
    usage.merge(&json!({"output_tokens":9}));
    assert_eq!(usage.total_input_tokens(), Some(34));
    assert_eq!(usage.output_tokens, Some(9));
}
