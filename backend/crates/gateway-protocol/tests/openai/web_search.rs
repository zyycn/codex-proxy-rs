//! Responses 工具选择中的强制搜索事实

use gateway_protocol::openai::web_search::is_required;
use serde_json::{Value, json};

#[test]
fn optional_or_mixed_tool_choices_do_not_require_search() {
    let search = json!({"type":"web_search"});
    let function = json!({"type":"function","name":"pwd"});
    for choice in [
        Value::Null,
        json!("auto"),
        json!("none"),
        json!("required"),
        function.clone(),
        json!({"type":"allowed_tools","mode":"auto","tools":[search]}),
        json!({"type":"allowed_tools","mode":"required","tools":[search,function]}),
        json!({"type":"allowed_tools","mode":"required","tools":[]}),
    ] {
        let body = json!({"tools":[search,function],"tool_choice":choice});
        let body = body.as_object().unwrap();
        assert!(!is_required(body), "{choice}");
    }
    assert!(!is_required(
        json!({"tool_choice":"required"}).as_object().unwrap()
    ));
}

#[test]
fn explicit_and_exclusive_tool_choices_require_search() {
    for kind in [
        "web_search",
        "web_search_preview",
        "web_search_2025_08_26",
        "web_search_preview_2025_03_11",
    ] {
        let search = json!({"type":kind,"filters":{"allowed_domains":["example.org"]},"external_web_access":false});
        for choice in [
            json!("required"),
            json!({"type":kind}),
            json!({"type":"allowed_tools","mode":"required","tools":[{"type":kind}]}),
        ] {
            let mut tools = vec![search.clone()];
            if choice != "required" {
                tools.push(json!({"type":"function","name":"pwd"}));
                tools.push(json!({"type":if kind == "web_search" {"web_search_preview"} else {"web_search"}}));
            }
            let body = json!({"tools":tools,"tool_choice":choice});
            let body = body.as_object().unwrap();
            assert!(is_required(body));
        }
    }
}
