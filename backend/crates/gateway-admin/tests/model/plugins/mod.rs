//! 验证插件元数据兼容新增字段，同时保留已知字段的类型约束

mod contract;
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
