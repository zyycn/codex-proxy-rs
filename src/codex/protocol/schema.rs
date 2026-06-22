//! Codex 协议所需的 JSON schema 纯处理逻辑。

use serde_json::{Map, Value};

pub(crate) struct PreparedSchema {
    pub(crate) schema: Value,
    pub(crate) original_schema: Option<Value>,
}

pub(crate) fn prepare_schema(schema: Value) -> PreparedSchema {
    let original_schema = has_tuple_schemas(&schema).then(|| schema.clone());
    let mut schema = schema;
    if original_schema.is_some() {
        convert_tuple_schemas(&mut schema);
    }
    inject_additional_properties(&mut schema);
    PreparedSchema {
        schema,
        original_schema,
    }
}

pub(crate) fn reconvert_tuple_values(data: Value, schema: &Value) -> Value {
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

fn convert_tuple_schemas(value: &mut Value) {
    let Value::Object(object) = value else {
        return;
    };

    if let Some(Value::Array(prefix_items)) = object.remove("prefixItems") {
        let mut properties = Map::new();
        let mut required = Vec::new();
        for (index, mut item) in prefix_items.into_iter().enumerate() {
            convert_tuple_schemas(&mut item);
            let key = index.to_string();
            properties.insert(key.clone(), item);
            required.push(Value::String(key));
        }
        object.insert("type".to_string(), Value::String("object".to_string()));
        object.insert("properties".to_string(), Value::Object(properties));
        object.insert("required".to_string(), Value::Array(required));
        object.insert("additionalProperties".to_string(), Value::Bool(false));
        object.remove("items");
        return;
    }

    convert_object_values(object, "properties");
    convert_object_value(object, "items");
    convert_schema_array(object, "oneOf");
    convert_schema_array(object, "anyOf");
    convert_schema_array(object, "allOf");
    convert_object_values(object, "$defs");
    convert_object_values(object, "definitions");
    convert_object_value(object, "if");
    convert_object_value(object, "then");
    convert_object_value(object, "else");
    convert_object_value(object, "not");
}

fn convert_object_value(object: &mut Map<String, Value>, key: &str) {
    if let Some(value) = object.get_mut(key) {
        convert_tuple_schemas(value);
    }
}

fn convert_object_values(object: &mut Map<String, Value>, key: &str) {
    let Some(Value::Object(values)) = object.get_mut(key) else {
        return;
    };
    for value in values.values_mut() {
        convert_tuple_schemas(value);
    }
}

fn convert_schema_array(object: &mut Map<String, Value>, key: &str) {
    let Some(Value::Array(values)) = object.get_mut(key) else {
        return;
    };
    for value in values {
        convert_tuple_schemas(value);
    }
}

fn inject_additional_properties(value: &mut Value) {
    let Value::Object(object) = value else {
        return;
    };
    if object.get("type").and_then(Value::as_str) == Some("object")
        && !object.contains_key("additionalProperties")
    {
        object.insert("additionalProperties".to_string(), Value::Bool(false));
    }

    inject_object_values(object, "properties");
    inject_object_value(object, "items");
    inject_schema_array(object, "oneOf");
    inject_schema_array(object, "anyOf");
    inject_schema_array(object, "allOf");
    inject_object_values(object, "$defs");
    inject_object_values(object, "definitions");
    inject_object_value(object, "if");
    inject_object_value(object, "then");
    inject_object_value(object, "else");
    inject_object_value(object, "not");
}

fn inject_object_value(object: &mut Map<String, Value>, key: &str) {
    if let Some(value) = object.get_mut(key) {
        inject_additional_properties(value);
    }
}

fn inject_object_values(object: &mut Map<String, Value>, key: &str) {
    let Some(Value::Object(values)) = object.get_mut(key) else {
        return;
    };
    for value in values.values_mut() {
        inject_additional_properties(value);
    }
}

fn inject_schema_array(object: &mut Map<String, Value>, key: &str) {
    let Some(Value::Array(values)) = object.get_mut(key) else {
        return;
    };
    for value in values {
        inject_additional_properties(value);
    }
}
