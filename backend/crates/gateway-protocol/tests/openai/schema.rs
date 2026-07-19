use gateway_protocol::openai::schema::reconvert_tuple_values;
use serde_json::json;

#[test]
fn tuple_reconversion_should_restore_nested_arrays_without_touching_other_properties() {
    let schema = json!({
        "type": "object",
        "properties": {
            "point": {
                "prefixItems": [
                    {"type": "number"},
                    {"type": "number"}
                ]
            },
            "label": {"type": "string"}
        }
    });
    let encoded = json!({"point": {"0": 12.5, "1": 9.0}, "label": "A"});

    assert_eq!(
        reconvert_tuple_values(encoded, &schema),
        json!({"point": [12.5, 9.0], "label": "A"})
    );
}

#[test]
fn tuple_reconversion_should_resolve_local_defs_reference() {
    let schema = json!({
        "$defs": {
            "pair": {
                "prefixItems": [
                    {"type": "string"},
                    {"type": "integer"}
                ]
            }
        },
        "properties": {
            "entry": {"$ref": "#/$defs/pair"}
        }
    });

    assert_eq!(
        reconvert_tuple_values(json!({"entry": {"0": "age", "1": 42}}), &schema),
        json!({"entry": ["age", 42]})
    );
}

#[test]
fn tuple_reconversion_should_recurse_through_homogeneous_arrays() {
    let schema = json!({
        "type": "array",
        "items": {
            "prefixItems": [
                {"type": "integer"},
                {"type": "integer"}
            ]
        }
    });

    assert_eq!(
        reconvert_tuple_values(json!([{"0": 1, "1": 2}, {"0": 3, "1": 4}]), &schema,),
        json!([[1, 2], [3, 4]])
    );
}

#[test]
fn tuple_reconversion_should_select_union_branch_containing_tuple_schema() {
    let schema = json!({
        "anyOf": [
            {"type": "string"},
            {"prefixItems": [{"type": "boolean"}, {"type": "string"}]}
        ]
    });

    assert_eq!(
        reconvert_tuple_values(json!({"0": true, "1": "ready"}), &schema),
        json!([true, "ready"])
    );
}

#[test]
fn tuple_reconversion_should_preserve_values_when_schema_does_not_describe_a_tuple() {
    let value = json!({"0": "not", "1": "a tuple", "extra": true});

    assert_eq!(
        reconvert_tuple_values(value.clone(), &json!({"type": "object"})),
        value
    );
}
