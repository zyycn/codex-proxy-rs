//! 验证插件元数据兼容新增字段，同时保留已知字段的类型约束

use gateway_admin::model::plugins::PluginArtifactMetadata;
use serde_json::{Value, json};

fn metadata() -> Value {
    json!({
        "pluginId": "test.example", "version": "1.0.0", "name": "example",
        "displayName": "Example", "publisher": "test", "description": "Example plugin",
        "license": "MIT", "sha256": "a".repeat(64), "platforms": ["linux-x86_64"],
        "contributes": {"middleware": {
            "id": "test.example.middleware", "version": 3,
            "stages": [], "inputFormats": [], "outputFormats": []
        }},
        "configurationSchema": {}, "secretFields": [], "stateNamespaces": []
    })
}

#[test]
fn metadata_ignores_unknown_fields_at_each_object_boundary() {
    let mut original = metadata();
    original["icon"] = json!({"light": "light.svg", "dark": "dark.svg"});
    original["stateNamespaces"] = json!([{
        "namespace": "cache", "schemaVersion": 1, "schemaSha256": "b".repeat(64),
        "schema": {}, "maximumRecords": 10, "maximumBytes": 1024,
        "maximumValueBytes": 128, "migratesFrom": []
    }]);
    let expected: PluginArtifactMetadata = serde_json::from_value(original.clone()).unwrap();
    for path in ["", "/contributes/middleware", "/icon", "/stateNamespaces/0"] {
        let mut extended = original.clone();
        extended.pointer_mut(path).unwrap()["futureField"] = json!({"enabled": true});
        assert_eq!(
            serde_json::from_value::<PluginArtifactMetadata>(extended).unwrap(),
            expected,
            "unknown field at {path}"
        );
    }
}

#[test]
fn metadata_defaults_only_optional_fields() {
    let original = metadata();
    let expected: PluginArtifactMetadata = serde_json::from_value(original.clone()).unwrap();
    let mut older = original;
    for field in ["configurationSchema", "secretFields", "stateNamespaces"] {
        older.as_object_mut().unwrap().remove(field);
    }
    for field in ["stages", "inputFormats", "outputFormats"] {
        older["contributes"]["middleware"]
            .as_object_mut()
            .unwrap()
            .remove(field);
    }
    assert_eq!(
        serde_json::from_value::<PluginArtifactMetadata>(older).unwrap(),
        expected
    );
}

#[test]
fn metadata_still_requires_identity_and_execution_facts() {
    for field in [
        "pluginId",
        "version",
        "name",
        "publisher",
        "sha256",
        "platforms",
        "contributes",
    ] {
        let mut incomplete = metadata();
        incomplete.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<PluginArtifactMetadata>(incomplete).is_err(),
            "missing {field}"
        );
    }
    for field in ["id", "version"] {
        let mut incomplete = metadata();
        incomplete["contributes"]["middleware"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            serde_json::from_value::<PluginArtifactMetadata>(incomplete).is_err(),
            "missing contribution {field}"
        );
    }
}

#[test]
fn metadata_still_rejects_wrong_types_in_known_fields() {
    for (path, value) in [
        ("/version", json!(1)),
        ("/secretFields", json!(false)),
        ("/stateNamespaces", json!({})),
        ("/contributes/middleware/version", json!("3")),
        ("/contributes/middleware/stages", json!("http")),
    ] {
        let mut invalid = metadata();
        *invalid.pointer_mut(path).unwrap() = value;
        assert!(
            serde_json::from_value::<PluginArtifactMetadata>(invalid).is_err(),
            "wrong type at {path}"
        );
    }
}
