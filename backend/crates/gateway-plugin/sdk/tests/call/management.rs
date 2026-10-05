//! 验证插件管理与命令行合同的正文分离、类型及权限字段约束

use gateway_plugin_sdk::call::management::{CommandRegistration, CommandResult, CommandValue};
use serde_json::json;

#[test]
fn management_contract_keeps_http_bodies_separate_and_rejects_authority_headers() {
    use gateway_plugin_sdk::call::management::{
        ManagementRegistration, ManagementRequest, ManagementResponse,
    };
    let registration: ManagementRegistration = serde_json::from_value(json!({
        "routes":[{"method":"POST","path":"report","response_content_types":["application/json"]}],
        "resources":[{"path":"index.html"}],"pages":[{"id":"report","title":"报告","entry":"index.html"}],
    })).unwrap();
    assert!(!registration.resources[0].public);
    assert!(registration.pages[0].description.is_none());
    let request =
        json!({"method":"POST","path":"report","query":"","content_type":"application/json"});
    assert!(serde_json::from_value::<ManagementRequest>(request.clone()).is_ok());
    let mut forged = request;
    forged["headers"] = json!({"Cookie":"must-not-cross-wire"});
    assert!(serde_json::from_value::<ManagementRequest>(forged).is_err());
    assert!(
        serde_json::from_value::<ManagementResponse>(
            json!({"status":200,"content_type":"text/html","set_cookie":"forged"})
        )
        .is_err()
    );
}

#[test]
fn management_page_description_round_trips_with_the_directory() {
    use gateway_plugin_sdk::call::management::ManagementPage;

    let page = json!({
        "id":"report", "title":"报告", "description":"查看请求处理状态",
        "entry":"index.html", "icon":null
    });
    let parsed: ManagementPage = serde_json::from_value(page.clone()).unwrap();
    assert_eq!(parsed.description.as_deref(), Some("查看请求处理状态"));
    assert_eq!(serde_json::to_value(parsed).unwrap(), page);
}

#[test]
fn command_values_preserve_types_and_portable_numeric_boundaries() {
    for value in [
        CommandValue::Bool(false),
        CommandValue::String("private-argument".into()),
        CommandValue::Int(i32::MIN),
        CommandValue::Int64(i64::MAX),
        CommandValue::Float64(0.25),
        CommandValue::Duration(-1_500_000_000),
    ] {
        let encoded = serde_json::to_vec(&value).unwrap();
        assert!(serde_json::from_slice::<CommandValue>(&encoded).unwrap() == value);
    }
    for invalid in [
        json!({"type":"int","value":2147483648_i64}),
        json!({"type":"duration","value":0.5}),
        json!({"type":"bool","value":"true"}),
        json!({"type":"string","value":"test","authority":"admin"}),
    ] {
        assert!(serde_json::from_value::<CommandValue>(invalid).is_err());
    }
}

#[test]
fn command_registration_and_result_reject_unknown_authority_fields() {
    let registration = json!({"commands":[{
        "name":"login", "description":"登录", "parameters":[{
            "name":"timeout", "description":"等待时长", "value_type":"duration",
            "default":{"type":"duration","value":30_000_000_000_i64}
        }]
    }]});
    let value: CommandRegistration = serde_json::from_value(registration.clone()).unwrap();
    assert_eq!(value.commands[0].name, "login");
    let mut invalid = registration;
    invalid["commands"][0]["admin_id"] = json!("forged");
    assert!(serde_json::from_value::<CommandRegistration>(invalid).is_err());
    let result: CommandResult =
        serde_json::from_value(json!({"stdout":"out\n","stderr":"err\n","exit_code":17})).unwrap();
    assert_eq!(result.exit_code, 17);
    assert!(result.accounts.is_empty());
    assert!(
        serde_json::from_value::<CommandResult>(json!({"stdout":"","stderr":"","exit_code":256}))
            .is_err()
    );
}
