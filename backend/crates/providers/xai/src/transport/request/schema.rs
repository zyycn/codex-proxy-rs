//! 工具 JSON Schema 校验与参数数值归一化。

use super::*;

/// 仅移除参数根节点的 null 分支，保持嵌套 schema 的业务语义。
pub(super) fn normalize_function_parameters_root(
    value: &Value,
) -> Result<Option<Value>, GrokRequestEncodeError> {
    let Some(schema) = value.as_object() else {
        return Ok(None);
    };
    let mut normalized = schema.clone();
    let mut changed = false;

    if let Some(types) = normalized.get("type").and_then(Value::as_array).cloned() {
        let removed_null = types.iter().any(|value| value.as_str() == Some("null"));
        if removed_null {
            let remaining = types
                .into_iter()
                .filter(|value| value.as_str() != Some("null"))
                .collect::<Vec<_>>();
            if remaining.len() != 1 || remaining[0].as_str() != Some("object") {
                return Err(invalid_function_parameters_root());
            }
            normalized.insert("type".to_owned(), Value::String("object".to_owned()));
            changed = true;
        }
    }

    for keyword in ["anyOf", "oneOf"] {
        let Some(branches) = normalized.get(keyword).and_then(Value::as_array).cloned() else {
            continue;
        };
        let removed_null = branches.iter().any(is_null_only_schema);
        if !removed_null {
            continue;
        }
        let remaining = branches
            .into_iter()
            .filter(|branch| !is_null_only_schema(branch))
            .collect::<Vec<_>>();
        if remaining.is_empty()
            || remaining.iter().any(|branch| {
                branch.as_object().is_none_or(|branch| {
                    !is_object_root_schema(branch, &normalized, &mut BTreeSet::new())
                })
            })
        {
            return Err(invalid_function_parameters_root());
        }
        if remaining.len() == 1 && normalized.len() == 1 {
            normalized = remaining[0]
                .as_object()
                .cloned()
                .ok_or_else(invalid_function_parameters_root)?;
            normalized.insert("type".to_owned(), Value::String("object".to_owned()));
        } else {
            normalized.insert(keyword.to_owned(), Value::Array(remaining));
            normalized.insert("type".to_owned(), Value::String("object".to_owned()));
        }
        changed = true;
    }

    Ok(changed.then_some(Value::Object(normalized)))
}

fn is_null_only_schema(value: &Value) -> bool {
    let Some(schema) = value.as_object() else {
        return false;
    };
    match schema.get("type") {
        Some(Value::String(kind)) => kind == "null",
        Some(Value::Array(types)) if !types.is_empty() => {
            types.iter().all(|kind| kind.as_str() == Some("null"))
        }
        _ => false,
    }
}

fn is_object_root_schema(
    schema: &Map<String, Value>,
    root: &Map<String, Value>,
    visited: &mut BTreeSet<String>,
) -> bool {
    match schema.get("type") {
        Some(Value::String(kind)) => return kind == "object",
        Some(Value::Array(types)) if !types.is_empty() => {
            return types.iter().all(|kind| kind.as_str() == Some("object"));
        }
        Some(_) => return false,
        None => {}
    }
    if schema.contains_key("properties") {
        return true;
    }
    let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
        return false;
    };
    if !visited.insert(reference.to_owned()) {
        return false;
    }
    resolve_local_schema_ref(root, reference)
        .is_some_and(|resolved| is_object_root_schema(resolved, root, visited))
}

fn resolve_local_schema_ref<'a>(
    root: &'a Map<String, Value>,
    reference: &str,
) -> Option<&'a Map<String, Value>> {
    if reference == "#" {
        return Some(root);
    }
    let path = reference.strip_prefix("#/")?;
    let mut segments = path.split('/');
    let first = decode_json_pointer_segment(segments.next()?);
    let mut current = root.get(&first)?;
    for segment in segments {
        let segment = decode_json_pointer_segment(segment);
        current = current.as_object()?.get(&segment)?;
    }
    current.as_object()
}

fn decode_json_pointer_segment(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}

pub(super) const fn invalid_function_parameters_root() -> GrokRequestEncodeError {
    GrokRequestEncodeError::InvalidRequestField {
        field: "tools[].parameters",
    }
}

pub(super) fn encode_function_arguments(
    value: Option<&Value>,
) -> Result<String, GrokRequestEncodeError> {
    match value {
        Some(Value::String(value)) => Ok(value.clone()),
        None | Some(Value::Null) => Ok("{}".to_owned()),
        Some(value) => serde_json::to_string(value)
            .map_err(|_| GrokRequestEncodeError::InvalidRequestNormalization),
    }
}

pub(super) fn normalize_function_arguments(arguments: &str, schema: &Value) -> Option<String> {
    if arguments.trim().is_empty() || !schema.is_object() {
        return None;
    }
    let mut value = serde_json::from_str::<Value>(arguments).ok()?;
    if !normalize_argument_value(&mut value, schema, schema, 0) {
        return None;
    }
    serde_json::to_string(&value).ok()
}

fn normalize_argument_value(value: &mut Value, schema: &Value, root: &Value, depth: usize) -> bool {
    if depth > 64 {
        return false;
    }
    let Some(schema) = schema.as_object() else {
        return false;
    };
    let mut changed = false;
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
        && let Some(resolved) = resolve_local_schema_value_ref(root, reference)
    {
        changed |= normalize_argument_value(value, resolved, root, depth + 1);
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
            for branch in branches {
                changed |= normalize_argument_value(value, branch, root, depth + 1);
            }
        }
    }
    if schema_requires_integer(schema)
        && let Value::Number(number) = value
        && let Some(normalized) = normalize_integral_number(number)
    {
        *number = normalized;
        return true;
    }
    match value {
        Value::Object(object) => {
            let properties = schema.get("properties").and_then(Value::as_object);
            let additional = schema
                .get("additionalProperties")
                .filter(|value| value.is_object());
            for (key, item) in object {
                let property = properties
                    .and_then(|properties| properties.get(key))
                    .or(additional);
                if let Some(property) = property {
                    changed |= normalize_argument_value(item, property, root, depth + 1);
                }
            }
        }
        Value::Array(items) => {
            let prefix = schema.get("prefixItems").and_then(Value::as_array);
            let item_schema = schema.get("items").filter(|value| value.is_object());
            for (index, item) in items.iter_mut().enumerate() {
                let schema = prefix
                    .and_then(|prefix| prefix.get(index))
                    .filter(|value| value.is_object())
                    .or(item_schema);
                if let Some(schema) = schema {
                    changed |= normalize_argument_value(item, schema, root, depth + 1);
                }
            }
        }
        _ => {}
    }
    changed
}

fn schema_requires_integer(schema: &Map<String, Value>) -> bool {
    match schema.get("type") {
        Some(Value::String(kind)) => kind == "integer",
        Some(Value::Array(kinds)) => {
            let mut integer = false;
            for kind in kinds.iter().filter_map(Value::as_str) {
                if kind == "number" {
                    return false;
                }
                integer |= kind == "integer";
            }
            integer
        }
        _ => false,
    }
}

fn normalize_integral_number(number: &serde_json::Number) -> Option<serde_json::Number> {
    let raw = number.to_string();
    if raw.len() > MAX_JSON_NUMBER_TEXT_BYTES || !raw.contains(['.', 'e', 'E']) {
        return None;
    }
    let (mantissa, exponent_text) = raw.find(['e', 'E']).map_or((raw.as_str(), ""), |index| {
        (&raw[..index], &raw[index + 1..])
    });
    let (negative, mantissa) = mantissa
        .strip_prefix('-')
        .map_or((false, mantissa), |value| (true, value));
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits = format!("{whole}{fraction}")
        .trim_start_matches('0')
        .to_owned();
    if digits.is_empty() {
        return "0".parse().ok().filter(|_| raw != "0");
    }
    let exponent = parse_bounded_decimal_exponent(exponent_text)?;
    let decimal_shift = exponent - i64::try_from(fraction.len()).ok()?;
    if decimal_shift < 0 {
        let fractional_digits = usize::try_from(-decimal_shift).ok()?;
        if fractional_digits > digits.len()
            || !digits[digits.len() - fractional_digits..]
                .bytes()
                .all(|byte| byte == b'0')
        {
            return None;
        }
        digits.truncate(digits.len() - fractional_digits);
        let trimmed = digits.trim_start_matches('0');
        if trimmed.is_empty() {
            return "0".parse().ok();
        }
        digits = trimmed.to_owned();
    } else if decimal_shift > 0 {
        let shift = usize::try_from(decimal_shift).ok()?;
        if shift
            > MAX_EXACT_JSON_INTEGER_TEXT
                .len()
                .saturating_sub(digits.len())
        {
            return None;
        }
        digits.extend(std::iter::repeat_n('0', shift));
    }
    if digits.len() > MAX_EXACT_JSON_INTEGER_TEXT.len()
        || (digits.len() == MAX_EXACT_JSON_INTEGER_TEXT.len()
            && digits.as_str() > MAX_EXACT_JSON_INTEGER_TEXT)
    {
        return None;
    }
    let normalized = if negative {
        format!("-{digits}")
    } else {
        digits
    };
    if normalized == raw {
        None
    } else {
        normalized.parse().ok()
    }
}

pub(super) fn exact_nonnegative_sequence(value: &Value) -> Option<u64> {
    let number = value.as_number()?;
    let sequence = number
        .as_u64()
        .or_else(|| normalize_integral_number(number)?.as_u64())?;
    (sequence <= MAX_EXACT_JSON_INTEGER).then_some(sequence)
}

fn parse_bounded_decimal_exponent(raw: &str) -> Option<i64> {
    if raw.is_empty() {
        return Some(0);
    }
    let (sign, digits) = if let Some(value) = raw.strip_prefix('+') {
        (1_i64, value)
    } else if let Some(value) = raw.strip_prefix('-') {
        (-1_i64, value)
    } else {
        (1_i64, raw)
    };
    if digits.is_empty() {
        return None;
    }
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some(0);
    }
    if digits.len() > 9 {
        return None;
    }
    digits.parse::<i64>().ok().map(|value| sign * value)
}

fn resolve_local_schema_value_ref<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    if reference == "#" {
        return Some(root);
    }
    let pointer = reference.strip_prefix('#')?;
    if !pointer.starts_with('/') {
        return None;
    }
    root.pointer(pointer)
}

pub(super) fn schema_contains_integer(schema: &Value, depth: usize) -> bool {
    let mut visited = BTreeSet::new();
    schema_contains_reachable_integer(schema, schema, &mut visited, depth)
}

fn schema_contains_reachable_integer(
    schema: &Value,
    root: &Value,
    visited_refs: &mut BTreeSet<String>,
    depth: usize,
) -> bool {
    if depth > 64 {
        return false;
    }
    let Some(schema) = schema.as_object() else {
        return false;
    };
    if schema_requires_integer(schema) {
        return true;
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
        && visited_refs.insert(reference.to_owned())
        && let Some(resolved) = resolve_local_schema_value_ref(root, reference)
        && schema_contains_reachable_integer(resolved, root, visited_refs, depth + 1)
    {
        return true;
    }
    for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if schema
            .get(keyword)
            .and_then(Value::as_array)
            .is_some_and(|branches| {
                branches.iter().any(|branch| {
                    schema_contains_reachable_integer(branch, root, visited_refs, depth + 1)
                })
            })
        {
            return true;
        }
    }
    for keyword in ["items", "additionalProperties"] {
        if schema.get(keyword).is_some_and(|child| {
            schema_contains_reachable_integer(child, root, visited_refs, depth + 1)
        }) {
            return true;
        }
    }
    schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| {
            properties.values().any(|property| {
                schema_contains_reachable_integer(property, root, visited_refs, depth + 1)
            })
        })
}
