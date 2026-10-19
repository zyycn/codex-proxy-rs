//! 验证路由模型标识与业务参数在 xAI 请求编码后保持原值

use super::*;

#[test]
fn routed_model_and_business_parameters_are_preserved() {
    let fields = json!({
        "presence_penalty": 0.1,
        "frequency_penalty": 0.2,
        "stop": ["done"],
        "logprobs": true,
        "top_logprobs": 5
    });
    let mut body = fields.clone();
    body["model"] = json!("client-model");
    body["input"] = json!("hello");
    let request = raw_request(body);

    for model in [
        "grok-4.7",
        "grok-latest",
        "grok-4.20-reasoning",
        "Grok-4.7",
        "xai/grok-4.7",
        "tenant/custom-model",
    ] {
        let encoded = GrokResponsesRequest::encode(&request, model, &client_key())
            .expect("routed upstream model");
        assert_eq!(encoded.body()["model"], model);
        for (key, value) in fields.as_object().expect("fields") {
            assert_eq!(encoded.body().get(key), Some(value), "{model} {key}");
        }
    }
}
