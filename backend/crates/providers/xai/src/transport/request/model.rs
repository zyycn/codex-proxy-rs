//! 当前 Responses 参数到 Grok 模型能力的转换。

use super::*;

pub(super) fn normalize_build_request(
    body: &mut Map<String, Value>,
    upstream_model: &str,
) -> Result<(), GrokRequestEncodeError> {
    apply_build_response_defaults(body)?;
    normalize_build_reasoning_effort(body, upstream_model);
    sanitize_build_model_fields(body, upstream_model);
    Ok(())
}

fn apply_build_response_defaults(
    body: &mut Map<String, Value>,
) -> Result<(), GrokRequestEncodeError> {
    if body.get("store").is_none_or(Value::is_null) {
        body.insert("store".to_owned(), Value::Bool(false));
    }

    let include = body
        .entry("include".to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    if include.is_null() {
        *include = Value::Array(Vec::new());
    }
    let include = include
        .as_array_mut()
        .ok_or(GrokRequestEncodeError::InvalidRequestField { field: "include" })?;
    if include.iter().any(|value| !value.is_string()) {
        return Err(GrokRequestEncodeError::InvalidRequestField { field: "include" });
    }
    if !include
        .iter()
        .any(|value| value.as_str() == Some("reasoning.encrypted_content"))
    {
        include.push(Value::String("reasoning.encrypted_content".to_owned()));
    }
    Ok(())
}

fn normalize_build_reasoning_effort(body: &mut Map<String, Value>, upstream_model: &str) {
    let supports_effort = grok_model_supports_reasoning_effort(upstream_model);
    if let Some(reasoning) = body.get_mut("reasoning").and_then(Value::as_object_mut) {
        if let Some(effort) = reasoning.remove("effort")
            && supports_effort
            && let Some(effort) = normalize_grok_reasoning_effort_value(&effort, upstream_model)
        {
            reasoning.insert("effort".to_owned(), Value::String(effort.to_owned()));
        }
        if reasoning.is_empty() {
            body.remove("reasoning");
        }
    }

    if is_grok_composer_model(upstream_model) {
        body.remove("reasoning");
    }
}

fn normalize_grok_reasoning_effort_value(value: &Value, model: &str) -> Option<&'static str> {
    let normalized = value.as_str()?.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "none" => Some("none"),
        "minimal" | "low" => Some("low"),
        "medium" => Some("medium"),
        "xhigh" if matches!(model, "grok-4.6" | "grok-4.6-latest") => Some("xhigh"),
        "high" | "xhigh" => Some("high"),
        _ => None,
    }
}

pub(super) fn strip_grok_provider_prefix(model: &str) -> &str {
    let model = model.trim();
    let lower = model.to_ascii_lowercase();
    ["xai/", "x-ai/", "grok/"]
        .into_iter()
        .find_map(|prefix| {
            lower
                .starts_with(prefix)
                .then(|| model[prefix.len()..].trim())
        })
        .unwrap_or(model)
}

pub(super) fn grok_model_last_slug(model: &str) -> String {
    model
        .trim()
        .rsplit_once('/')
        .map_or_else(|| model.trim(), |(_, slug)| slug.trim())
        .to_ascii_lowercase()
}

pub(super) fn is_grok_composer_model(model: &str) -> bool {
    matches!(
        grok_model_last_slug(model).as_str(),
        "grok-composer" | "grok-composer-2.5-fast" | "composer-2.5"
    )
}

fn grok_model_supports_reasoning_effort(model: &str) -> bool {
    matches!(
        strip_grok_provider_prefix(model)
            .to_ascii_lowercase()
            .as_str(),
        "grok-4.5"
            | "grok-4.5-latest"
            | "grok-4.6"
            | "grok-4.6-latest"
            | "grok-4.3"
            | "grok-4.3-latest"
            | "grok-3-mini"
            | "grok-3-mini-fast"
            | "grok-4.20-0309-reasoning"
            | "grok-4.20-reasoning"
            | "grok-4.20-multi-agent-0309"
    )
}

fn sanitize_build_model_fields(body: &mut Map<String, Value>, upstream_model: &str) {
    for field in ["prompt_cache_retention", "safety_identifier"] {
        body.remove(field);
    }
    if upstream_model.trim().eq_ignore_ascii_case("grok-4.5") {
        for field in [
            "presence_penalty",
            "presencePenalty",
            "frequency_penalty",
            "frequencyPenalty",
            "stop",
        ] {
            body.remove(field);
        }
    }
    if ["grok-4.20", "grok-4.3", "grok-4.5", "grok-4.6"]
        .iter()
        .any(|prefix| grok_model_last_slug(upstream_model).starts_with(prefix))
    {
        body.remove("logprobs");
        body.remove("top_logprobs");
    }
    body.remove("external_web_access");
}
