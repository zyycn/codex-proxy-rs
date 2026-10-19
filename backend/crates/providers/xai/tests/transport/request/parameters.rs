//! 验证推理档位遵守 Responses wire 而不由模型 slug 猜测

use super::*;

#[test]
fn official_reasoning_efforts_are_preserved_for_current_and_future_models() {
    for model in ["grok-4.7", "grok-future", "tenant/custom-model"] {
        for effort in ["none", "minimal", "low", "medium", "high", "xhigh", "max"] {
            let request = raw_request(json!({
                "input": "hello",
                "reasoning": {"effort": effort, "summary": "auto"}
            }));
            let encoded = GrokResponsesRequest::encode(&request, model, &client_key())
                .expect("official effort wire");
            assert_eq!(
                encoded.body()["reasoning"],
                json!({"effort": effort, "summary": "auto"}),
                "model {model}"
            );
        }
    }
}

#[test]
fn invalid_reasoning_effort_is_rejected_instead_of_silently_dropped() {
    for effort in [json!("future-value"), json!(42), json!({}), json!([])] {
        let request = raw_request(json!({"reasoning": {"effort": effort}}));
        assert!(
            matches!(
                GrokResponsesRequest::encode(&request, "grok-4.7", &client_key()),
                Err(GrokRequestEncodeError::InvalidRequestField {
                    field: "reasoning.effort"
                })
            ),
            "effort {effort}"
        );
    }
}

#[test]
fn reasoning_effort_keeps_existing_case_and_whitespace_normalization() {
    let request = raw_request(json!({"reasoning": {"effort": " XHIGH "}}));
    let encoded = GrokResponsesRequest::encode(&request, "grok-4.7", &client_key())
        .expect("normalized official effort");
    assert_eq!(encoded.body()["reasoning"]["effort"], "xhigh");
}

#[test]
fn reasoning_shape_is_validated_while_absent_effort_uses_server_default() {
    for reasoning in [
        Value::Null,
        json!({}),
        json!({"effort": null}),
        json!({"summary":"auto"}),
    ] {
        let request = raw_request(json!({"reasoning": reasoning}));
        let encoded = GrokResponsesRequest::encode(&request, "grok-4.7", &client_key())
            .expect("no explicit effort");
        assert_eq!(encoded.body().get("reasoning"), Some(&reasoning));
    }
    for reasoning in [json!("high"), json!(42), json!([])] {
        let request = raw_request(json!({"reasoning": reasoning}));
        assert!(matches!(
            GrokResponsesRequest::encode(&request, "grok-4.7", &client_key()),
            Err(GrokRequestEncodeError::InvalidRequestField { field: "reasoning" })
        ));
    }
}
