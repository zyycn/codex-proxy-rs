//! Google 原生请求的能力与思考配置

use serde_json::{Map, Value};

use crate::capabilities::RequestFacts;

pub(crate) fn request_facts(body: &Map<String, Value>, facts: &mut RequestFacts) {
    let config = generation_config(body);
    let formats = field(config, "responseFormat", "response_format").and_then(Value::as_array);
    facts.max_output_tokens =
        field(config, "maxOutputTokens", "max_output_tokens").and_then(Value::as_u64);
    facts.reasoning = thinking_config(body).is_some_and(Value::is_object);
    facts.json_schema = field(config, "responseSchema", "response_schema")
        .is_some_and(Value::is_object)
        || field(config, "responseJsonSchema", "response_json_schema")
            .is_some_and(|v| !v.is_null())
        || formats.is_some_and(|formats| {
            formats
                .iter()
                .any(|format| format.pointer("/text/schema").is_some_and(|v| !v.is_null()))
        });
    facts.image_generation = field(config, "responseModalities", "response_modalities")
        .and_then(Value::as_array)
        .is_some_and(|modes| {
            modes
                .iter()
                .any(|mode| mode.as_str() == Some("IMAGE") || mode.as_u64() == Some(2))
        })
        || field(config, "imageConfig", "image_config").is_some_and(Value::is_object)
        || formats.is_some_and(|formats| {
            formats
                .iter()
                .any(|format| format.get("image").is_some_and(Value::is_object))
        });
    facts.vision = body
        .get("contents")
        .and_then(Value::as_array)
        .is_some_and(|contents| {
            contents
                .iter()
                .filter_map(|content| content.get("parts")?.as_array())
                .flatten()
                .any(|part| {
                    part_has_image(part)
                        || field(Some(part), "functionResponse", "function_response")
                            .and_then(|response| response.get("parts")?.as_array())
                            .is_some_and(|parts| parts.iter().any(part_has_image))
                })
        });
}

pub(crate) fn thinking_level(body: &Map<String, Value>) -> Option<&str> {
    field(thinking_config(body), "thinkingLevel", "thinking_level")?
        .as_str()
        .map(str::trim)
        .filter(|level| !level.is_empty())
}

fn generation_config(body: &Map<String, Value>) -> Option<&Value> {
    body.get("generationConfig")
        .or_else(|| body.get("generation_config"))
}

fn thinking_config(body: &Map<String, Value>) -> Option<&Value> {
    field(generation_config(body), "thinkingConfig", "thinking_config")
}

fn part_has_image(part: &Value) -> bool {
    let blob = field(Some(part), "inlineData", "inline_data")
        .or_else(|| field(Some(part), "fileData", "file_data"));
    field(blob, "mimeType", "mime_type")
        .and_then(Value::as_str)
        .is_some_and(|mime| {
            mime.get(..6)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image/"))
        })
}

fn field<'a>(value: Option<&'a Value>, json: &str, proto: &str) -> Option<&'a Value> {
    let object = value?.as_object()?;
    object.get(json).or_else(|| object.get(proto))
}
