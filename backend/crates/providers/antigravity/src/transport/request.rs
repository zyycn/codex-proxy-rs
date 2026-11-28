//! 合成系统提示并改写 Cloud Code 身份字段，其他字段保留原始 JSON 字节

use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor},
};
use serde_json::value::RawValue;

use super::unsent;
use gateway_core::error::{ProviderError, ProviderErrorKind};

pub(super) fn request_body(
    body: &[u8],
    instruction: Option<&RawValue>,
    count: bool,
) -> Result<Box<RawValue>, ProviderError> {
    let rewrite = || {
        let raw: &RawValue = serde_json::from_slice(body)?;
        let instruction = instruction
            .map(|prompt| prepend_system_prompt(raw, prompt))
            .transpose()?;
        edit_object(
            raw,
            |name, value| {
                if (instruction.is_some()
                    && matches!(name, "systemInstruction" | "system_instruction"))
                    || (count
                        && matches!(
                            name,
                            "model"
                                | "safetySettings"
                                | "safety_settings"
                                | "toolConfig"
                                | "tool_config"
                                | "labels"
                                | "sessionId"
                                | "session_id"
                        ))
                {
                    return Ok(Edit::Remove);
                }
                if name == "labels" && value.get().starts_with('{') {
                    // CLI Gateway 添加的产品归属标识不属于个人 OAuth 身份；会话和业务标签保留
                    return edit_object(
                        value,
                        |label, _| {
                            Ok(
                                if matches!(
                                    label,
                                    "client"
                                        | "goog-originating-logical-product-id"
                                        | "goog-originating-service-id"
                                ) {
                                    Edit::Remove
                                } else {
                                    Edit::Keep
                                },
                            )
                        },
                        None,
                    )
                    .map(Edit::Replace);
                }
                Ok(Edit::Keep)
            },
            instruction
                .as_deref()
                .map(|value| ("systemInstruction", value)),
        )
    };
    rewrite().map_err(|error| unsent(ProviderErrorKind::InvalidRequest).with_source(error))
}

fn prepend_system_prompt(
    request: &RawValue,
    prompt: &RawValue,
) -> Result<Box<RawValue>, serde_json::Error> {
    let Fields(fields) = serde_json::from_str(request.get())?;
    let client = ["systemInstruction", "system_instruction"]
        .into_iter()
        .find_map(|key| {
            fields
                .iter()
                .rev()
                .find(|(name, _)| name == key)
                .map(|(_, value)| *value)
        });
    let Some(client) = client.filter(|value| value.get() != "null") else {
        return Ok(prompt.to_owned());
    };
    #[derive(Deserialize)]
    struct Content<'a> {
        #[serde(default, borrow)]
        parts: Vec<&'a RawValue>,
    }
    let mut parts = serde_json::from_str::<Content<'_>>(prompt.get())?.parts;
    // 客户端系统指令还包含规则、工具说明和工作区上下文，不能按身份提示整体替换
    parts.extend(serde_json::from_str::<Content<'_>>(client.get())?.parts);
    let parts = serde_json::value::to_raw_value(&parts)?;
    edit_object(
        client,
        |name, _| {
            Ok(if name == "parts" {
                Edit::Remove
            } else {
                Edit::Keep
            })
        },
        Some(("parts", &parts)),
    )
}

enum Edit {
    Keep,
    Remove,
    Replace(Box<RawValue>),
}

fn edit_object(
    body: &RawValue,
    mut edit: impl FnMut(&str, &RawValue) -> Result<Edit, serde_json::Error>,
    append: Option<(&str, &RawValue)>,
) -> Result<Box<RawValue>, serde_json::Error> {
    let json = body.get();
    let Fields(fields) = serde_json::from_str(json)?;
    let mut output = String::with_capacity(json.len());
    output.push('{');
    let mut cursor = 1;
    let mut retained = false;
    for (name, value) in fields {
        // RawValue 借自同一输入；位置由 serde JSON 解析器确定，不自行解析值或转码数字
        let start = value.get().as_ptr() as usize - json.as_ptr() as usize;
        let prefix = &json[cursor..start];
        cursor = start + value.get().len();
        let replacement = match edit(&name, value)? {
            Edit::Keep => None,
            Edit::Remove => continue,
            Edit::Replace(value) => Some(value),
        };
        let comma = (!retained)
            .then(|| prefix.bytes().position(|byte| !byte.is_ascii_whitespace()))
            .flatten()
            .filter(|&index| prefix.as_bytes()[index] == b',');
        if let Some(index) = comma {
            output.push_str(&prefix[..index]);
            output.push_str(&prefix[index + 1..]);
        } else {
            output.push_str(prefix);
        }
        output.push_str(replacement.as_deref().unwrap_or(value).get());
        retained = true;
    }
    if let Some((name, value)) = append {
        if retained {
            output.push(',');
        }
        output.push_str(&serde_json::to_string(name)?);
        output.push(':');
        output.push_str(value.get());
    }
    output.push_str(&json[cursor..]);
    RawValue::from_string(output)
}

// 保留顺序和重复字段；身份字段的重复项也必须全部处理，不能被 Map 的最后值语义遗漏
struct Fields<'a>(Vec<(String, &'a RawValue)>);

impl<'de> Deserialize<'de> for Fields<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = Fields<'de>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut fields = Vec::new();
                while let Some(name) = map.next_key()? {
                    fields.push((name, map.next_value()?));
                }
                Ok(Fields(fields))
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}
