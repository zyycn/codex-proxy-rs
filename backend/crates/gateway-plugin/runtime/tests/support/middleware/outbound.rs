//! 真实出站准备插件，记录洋葱顺序并测试完整请求与调用资源

use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::middleware::NEXT_METHOD,
    client::{OutboundCall, OutboundRequest, SessionError},
};

pub(super) async fn handle(
    mut call: OutboundCall,
    configuration: serde_json::Value,
) -> Result<OutboundRequest, PluginFault> {
    assert_eq!(call.metadata.provider, "openai");
    assert_eq!(call.metadata.account_id, "acct_openai");
    assert_eq!(call.metadata.attempt_index, 1);
    assert_eq!(call.metadata.client_key_id, "key-middleware");
    let record = |phase| {
        if let Some(path) = configuration["records"].as_str() {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(file, "{phase}:{}", call.context.instance_id).unwrap();
        }
    };
    record("enter");
    if let Some(expected) = configuration.get("expected_body") {
        assert_eq!(serde_json::json!(call.request.body), *expected);
    }
    if let Some(headers) = configuration.get("headers") {
        call.request.head.headers = serde_json::from_value(headers.clone()).unwrap();
    }
    if let Some(target) = configuration.get("target") {
        call.request.head.target = serde_json::from_value(target.clone()).unwrap();
    }
    if let Some(url) = configuration["http_url"].as_str() {
        let body_len = configuration["http_body_len"].as_u64().unwrap_or(0) as usize;
        call.host
            .call(
                "host.http.do",
                serde_json::json!({"method":"POST", "url":url}),
                vec![42; body_len],
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
    }
    if configuration["fault"] == true {
        return Err(PluginFault::new(ErrorCode::Fault, "outbound fixture error"));
    }
    if let Some(byte) = configuration["enter_byte"].as_u64() {
        call.request.body.push(byte as u8);
    }
    let mut request = if configuration["direct"] == true {
        call.request
    } else {
        call.next.run(call.request).await?
    };
    if configuration["second_next"] == true {
        let result = call
            .host
            .call(
                NEXT_METHOD,
                serde_json::to_value(&request.head).unwrap(),
                request.body.clone(),
            )
            .await;
        let Err(error) = result else {
            panic!("next can only be consumed once")
        };
        assert_eq!(error.into_plugin_fault().code, ErrorCode::InvalidInput);
    }
    if let Some(byte) = configuration["exit_byte"].as_u64() {
        request.body.push(byte as u8);
    }
    record("exit");
    Ok(request)
}
