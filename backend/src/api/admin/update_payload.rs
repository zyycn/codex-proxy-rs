//! 管理端通用更新请求解析。

use serde::Deserialize;
use serde_json::Value;

use crate::api::admin::response::AdminError;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EditableUpdateRequest {
    id: String,
    label: Option<Option<String>>,
    status: Option<String>,
}

pub(crate) struct ParsedEditableUpdate {
    pub id: String,
    pub label: Option<Option<String>>,
    pub status: Option<String>,
}

pub(crate) struct EditableUpdateMessages<'a> {
    pub object_required: &'a str,
    pub invalid: &'a str,
    pub empty_update: &'a str,
    pub unknown_field_editable: bool,
}

pub(crate) fn parse_editable_update(
    payload: &Value,
    messages: EditableUpdateMessages<'_>,
) -> Result<ParsedEditableUpdate, AdminError> {
    let object = payload
        .as_object()
        .ok_or_else(|| AdminError::bad_request(messages.object_required))?;
    if messages.unknown_field_editable {
        for field in object.keys() {
            if !matches!(field.as_str(), "id" | "label" | "status") {
                return Err(AdminError::bad_request(format!("{field} is not editable")));
            }
        }
    }

    let update = serde_json::from_value::<EditableUpdateRequest>(payload.clone())
        .map_err(|_| AdminError::bad_request(messages.invalid))?;
    if is_blank(&update.id) || update.status.as_deref().is_some_and(is_blank) {
        return Err(AdminError::bad_request(messages.invalid));
    }
    if update.label.is_none() && update.status.is_none() {
        return Err(AdminError::bad_request(messages.empty_update));
    }

    Ok(ParsedEditableUpdate {
        id: update.id,
        label: update.label,
        status: update.status,
    })
}

fn is_blank(value: &str) -> bool {
    value.trim().is_empty()
}
