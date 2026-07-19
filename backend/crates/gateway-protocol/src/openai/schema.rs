//! Codex 协议所需的 JSON schema 纯处理逻辑。

use serde_json::{Map, Value};

pub fn reconvert_tuple_values(data: Value, schema: &Value) -> Value {
    reconvert_tuple_values_with_root(data, schema, schema)
}

fn reconvert_tuple_values_with_root(data: Value, schema: &Value, root_schema: &Value) -> Value {
    let Value::Object(schema_object) = schema else {
        return data;
    };

    if let Some(reference) = schema_object.get("$ref").and_then(Value::as_str) {
        let Some(resolved) = resolve_ref(reference, root_schema) else {
            return data;
        };
        return reconvert_tuple_values_with_root(data, resolved, root_schema);
    }

    if let Some(Value::Array(prefix_items)) = schema_object.get("prefixItems") {
        let mut data_object = match data {
            Value::Object(data_object) => data_object,
            other => return other,
        };
        let values = prefix_items
            .iter()
            .enumerate()
            .map(|(index, item_schema)| {
                let value = data_object
                    .remove(&index.to_string())
                    .unwrap_or(Value::Null);
                reconvert_tuple_values_with_root(value, item_schema, root_schema)
            })
            .collect();
        return Value::Array(values);
    }

    if let Some(Value::Object(properties)) = schema_object.get("properties") {
        let mut data_object = match data {
            Value::Object(data_object) => data_object,
            other => return other,
        };
        for (key, property_schema) in properties {
            if let Some(value) = data_object.remove(key) {
                data_object.insert(
                    key.clone(),
                    reconvert_tuple_values_with_root(value, property_schema, root_schema),
                );
            }
        }
        return Value::Object(data_object);
    }

    if let Some(items_schema) = schema_object.get("items") {
        let values = match data {
            Value::Array(values) => values,
            other => return other,
        };
        return Value::Array(
            values
                .into_iter()
                .map(|value| reconvert_tuple_values_with_root(value, items_schema, root_schema))
                .collect(),
        );
    }

    for key in ["oneOf", "anyOf", "allOf"] {
        let Some(Value::Array(branches)) = schema_object.get(key) else {
            continue;
        };
        let Some(branch) = branches.iter().find(|branch| has_tuple_schemas(branch)) else {
            continue;
        };
        return reconvert_tuple_values_with_root(data, branch, root_schema);
    }

    data
}

fn resolve_ref<'a>(reference: &str, root_schema: &'a Value) -> Option<&'a Value> {
    let path = reference
        .strip_prefix("#/$defs/")
        .map(|path| ("$defs", path))
        .or_else(|| {
            reference
                .strip_prefix("#/definitions/")
                .map(|path| ("definitions", path))
        })?;
    root_schema.get(path.0)?.get(path.1)
}

fn has_tuple_schemas(value: &Value) -> bool {
    let Value::Object(object) = value else {
        return false;
    };
    if object
        .get("prefixItems")
        .is_some_and(serde_json::Value::is_array)
    {
        return true;
    }

    object_values_have_tuple_schema(object, "properties")
        || object.get("items").is_some_and(has_tuple_schemas)
        || schema_array_has_tuple_schema(object, "oneOf")
        || schema_array_has_tuple_schema(object, "anyOf")
        || schema_array_has_tuple_schema(object, "allOf")
        || object_values_have_tuple_schema(object, "$defs")
        || object_values_have_tuple_schema(object, "definitions")
        || object.get("if").is_some_and(has_tuple_schemas)
        || object.get("then").is_some_and(has_tuple_schemas)
        || object.get("else").is_some_and(has_tuple_schemas)
        || object.get("not").is_some_and(has_tuple_schemas)
}

fn object_values_have_tuple_schema(object: &Map<String, Value>, key: &str) -> bool {
    let Some(Value::Object(values)) = object.get(key) else {
        return false;
    };
    values.values().any(has_tuple_schemas)
}

fn schema_array_has_tuple_schema(object: &Map<String, Value>, key: &str) -> bool {
    let Some(Value::Array(values)) = object.get(key) else {
        return false;
    };
    values.iter().any(has_tuple_schemas)
}
