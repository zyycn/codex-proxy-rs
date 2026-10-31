//! 模型目录推理档位与可见摘要能力的独立声明及缺省兼容测试

use gateway_plugin_sdk::call::catalog::ModelPresentation;
use serde_json::json;

#[test]
fn reasoning_efforts_do_not_imply_summary_support() {
    for summary in [None, Some(false), Some(true)] {
        let mut value = json!({
            "default_reasoning_effort":"low",
            "supported_reasoning_efforts":["low","high"]
        });
        if let Some(summary) = summary {
            value["reasoning_summary"] = summary.into();
        }
        let presentation: ModelPresentation = serde_json::from_value(value).unwrap();
        assert_eq!(
            presentation.default_reasoning_effort.as_deref(),
            Some("low")
        );
        assert_eq!(presentation.supported_reasoning_efforts, ["low", "high"]);
        assert_eq!(presentation.reasoning_summary, summary.unwrap_or(false));
        let encoded = serde_json::to_value(&presentation).unwrap();
        assert_eq!(encoded["reasoning_summary"], summary.unwrap_or(false));
        assert_eq!(
            serde_json::from_value::<ModelPresentation>(encoded).unwrap(),
            presentation
        );
    }
}
