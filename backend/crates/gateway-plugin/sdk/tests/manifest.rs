//! 验证插件清单的默认值、路径安全、平台与能力版本约束

use gateway_plugin_sdk::{
    Capability, ContributionDeclaration, Engines, Handshake, MANIFEST_VERSION, Manifest,
    ManifestError, PROTOCOL_VERSION, Package, PackageTarget, PluginIcon, PluginIconVariants, Stage,
    call::registration::Registration, valid_package_path,
};
use serde_json::json;

fn source_manifest() -> Manifest {
    serde_json::from_value(json!({
        "manifestVersion":2,
        "name": "request-tags",
        "displayName": "请求标签",
        "publisher": "9acme",
        "version": "1.0.0",
        "description": "测试插件",
        "license": "MIT",
        "author": "test",
        "engines": {"codex-proxy-rs": "*"},
        "main": "bin/worker",
        "runtime": "trustedProcess",
        "contributes": {
            "middleware": {
                "id": "9acme.request-tags.tagRequest",
                "version": 4,
                "stages": ["request"],
                "inputFormats": ["openai"],
                "outputFormats": ["openai"]
            }
        }
    }))
    .unwrap()
}

fn packaged_manifest() -> Manifest {
    let mut manifest = source_manifest();
    manifest.engines = Engines {
        codex_proxy_rs: ">=1.0.0, <2.0.0".parse().unwrap(),
    };
    manifest.package = Some(Package {
        protocol_version: PROTOCOL_VERSION,
        target: PackageTarget {
            os: "linux".into(),
            architecture: "x86_64".into(),
        },
        files: [("bin/worker".into(), "a".repeat(64))]
            .into_iter()
            .collect(),
    });
    manifest
}

#[test]
fn package_paths_reject_platform_escapes_and_ambiguous_names() {
    for invalid in [
        "/tmp/code",
        "../code",
        "a/../b",
        "a/./b",
        "a//b",
        "C:/code",
        "a\\b",
        "NUL.txt",
        "aux",
        "bin/a.",
        "bin/a ",
    ] {
        assert!(!valid_package_path(invalid), "{invalid}");
    }
    assert!(valid_package_path("bin/example-worker.exe"));
}

#[test]
fn source_manifest_allows_missing_package_and_wildcard_engine() {
    let manifest = source_manifest();
    assert_eq!(manifest.plugin_id().unwrap(), "9acme.request-tags");
    assert!(manifest.validate().is_ok());
    assert_eq!(
        manifest.package_for(&"1.2.0".parse().unwrap(), "linux", "x86_64"),
        Err(ManifestError::Invalid)
    );
}

#[test]
fn author_manifest_fills_default_ids_versions_and_fixed_stages() {
    let source = json!({
        "manifestVersion": MANIFEST_VERSION,
        "name": "request-tags",
        "displayName": "请求标签",
        "publisher": "9acme",
        "version": "1.0.0",
        "description": "测试插件",
        "license": "MIT",
        "engines": {"codex-proxy-rs": "*"},
        "main": "bin/worker",
        "runtime": "trustedProcess",
        "contributes": {
            "middleware": {"version": 4, "stages": ["attempt"]},
            "management": {}
        }
    });
    let manifest = Manifest::from_author_slice(&serde_json::to_vec(&source).unwrap()).unwrap();
    let middleware = &manifest.contributes[&Capability::Middleware];
    let management = &manifest.contributes[&Capability::Management];

    assert_eq!(middleware.id, "9acme.request-tags.middleware");
    assert_eq!(middleware.version, 4);
    assert_eq!(middleware.stages, [Stage::Attempt]);
    assert_eq!(management.id, "9acme.request-tags.management");
    assert_eq!(management.version, 1);
    assert_eq!(management.stages, [Stage::Management]);
}

#[test]
fn installation_checks_protocol_engine_and_target() {
    let mut manifest = packaged_manifest();
    let host = "1.2.0".parse().unwrap();
    assert!(manifest.package_for(&host, "linux", "x86_64").is_ok());
    assert_eq!(
        manifest.package_for(&host, "windows", "aarch64"),
        Err(ManifestError::Platform)
    );
    manifest.package.as_mut().unwrap().protocol_version = PROTOCOL_VERSION + 1;
    assert_eq!(manifest.validate(), Err(ManifestError::Incompatible));
    manifest.package.as_mut().unwrap().protocol_version = PROTOCOL_VERSION;
    manifest.engines.codex_proxy_rs = "*".parse().unwrap();
    assert_eq!(manifest.validate(), Err(ManifestError::Invalid));
}

#[test]
fn capability_contract_versions_are_checked_independently_of_rpc_version() {
    let mut manifest = packaged_manifest();
    assert!(manifest.validate().is_ok());
    for version in [0, 1, 2, 5, u32::MAX] {
        manifest
            .contributes
            .get_mut(&Capability::Middleware)
            .unwrap()
            .version = version;
        assert_eq!(manifest.validate(), Err(ManifestError::Invalid));
    }

    manifest.contributes.clear();
    manifest.contributes.insert(
        Capability::Management,
        ContributionDeclaration {
            id: "9acme.request-tags.management".into(),
            version: 2,
            stages: vec![Stage::Management],
            input_formats: vec![],
            output_formats: vec![],
        },
    );
    assert_eq!(manifest.validate(), Err(ManifestError::Invalid));
}

#[test]
fn package_rejects_file_directory_conflicts_and_missing_declared_files() {
    let mut manifest = packaged_manifest();
    manifest
        .package
        .as_mut()
        .unwrap()
        .files
        .insert("bin".into(), "b".repeat(64));
    assert_eq!(manifest.validate(), Err(ManifestError::Invalid));

    let mut manifest = packaged_manifest();
    manifest
        .resources
        .insert("ui/index.html".into(), "text/html".into());
    assert_eq!(manifest.validate(), Err(ManifestError::Invalid));

    let mut manifest = packaged_manifest();
    manifest.main = "bin/missing".into();
    assert_eq!(manifest.validate(), Err(ManifestError::Invalid));
}

#[test]
fn icon_accepts_declared_image_resources_and_rejects_unsafe_shapes() {
    let mut manifest = source_manifest();
    manifest
        .resources
        .insert("assets/icon.png".into(), "image/png".into());
    manifest.icon = Some(PluginIcon::Path("assets/icon.png".into()));
    assert!(manifest.validate().is_ok());

    manifest
        .resources
        .insert("assets/icon-dark.webp".into(), "image/webp".into());
    manifest.icon = Some(PluginIcon::Themed(PluginIconVariants {
        light: "assets/icon.png".into(),
        dark: "assets/icon-dark.webp".into(),
    }));
    assert!(manifest.validate().is_ok());

    manifest.icon = Some(PluginIcon::Path("assets/missing.png".into()));
    assert_eq!(manifest.validate(), Err(ManifestError::Invalid));
    manifest
        .resources
        .insert("assets/icon.svg".into(), "image/svg+xml".into());
    manifest.icon = Some(PluginIcon::Path("assets/icon.svg".into()));
    assert!(manifest.validate().is_ok());
    manifest.icon = Some(PluginIcon::Path("assets/icon.png".into()));
    manifest
        .resources
        .insert("assets/icon.png".into(), "image/jpeg".into());
    assert_eq!(manifest.validate(), Err(ManifestError::Invalid));
}

#[test]
fn icon_accepts_common_extensions_and_mime_aliases() {
    for (extension, mime) in [
        ("SVG", "image/svg+xml"),
        ("PNG", "image/png"),
        ("jpeg", "image/jpeg"),
        ("jfif", "image/jpeg"),
        ("WebP", "image/webp"),
        ("gif", "image/gif"),
        ("ico", "image/x-icon"),
        ("ICO", "image/vnd.microsoft.icon"),
        ("bmp", "image/bmp"),
        ("BMP", "image/x-ms-bmp"),
    ] {
        let mut manifest = source_manifest();
        let path = format!("assets/icon.{extension}");
        manifest.resources.insert(path.clone(), mime.into());
        manifest.icon = Some(PluginIcon::Path(path));
        assert!(manifest.validate().is_ok(), "{extension}: {mime}");
    }
}

#[test]
fn icon_rejects_non_image_resources_even_when_declared() {
    for (path, mime) in [
        ("assets/icon.html", "text/html"),
        ("assets/icon.svg", "text/xml"),
    ] {
        let mut manifest = source_manifest();
        manifest.resources.insert(path.into(), mime.into());
        manifest.icon = Some(PluginIcon::Path(path.into()));
        assert_eq!(manifest.validate(), Err(ManifestError::Invalid));
    }
}

#[test]
fn icon_wire_is_a_path_or_exact_light_dark_object() {
    let mut manifest = source_manifest();
    manifest
        .resources
        .insert("assets/icon.png".into(), "image/png".into());
    manifest.icon = Some(PluginIcon::Path("assets/icon.png".into()));
    assert_eq!(
        serde_json::to_value(&manifest).unwrap()["icon"],
        json!("assets/icon.png")
    );

    let mut value = serde_json::to_value(&manifest).unwrap();
    value["icon"] = json!({
        "light": "assets/icon.png",
        "dark": "assets/icon.png",
        "builtin": "puzzle"
    });
    assert!(serde_json::from_value::<Manifest>(value).is_err());
}

#[test]
fn contribution_ids_are_owned_unique_and_stage_bounded() {
    let mut manifest = source_manifest();
    let middleware = manifest
        .contributes
        .get(&Capability::Middleware)
        .unwrap()
        .clone();
    manifest
        .contributes
        .insert(Capability::Scheduler, middleware);
    assert_eq!(manifest.validate(), Err(ManifestError::Invalid));

    let mut manifest = source_manifest();
    manifest
        .contributes
        .get_mut(&Capability::Middleware)
        .unwrap()
        .id = "other.request-tags.tagRequest".into();
    assert_eq!(manifest.validate(), Err(ManifestError::Invalid));

    let mut manifest = source_manifest();
    manifest
        .contributes
        .get_mut(&Capability::Middleware)
        .unwrap()
        .stages = vec![Stage::Registration];
    assert_eq!(manifest.validate(), Err(ManifestError::Invalid));
}

#[test]
fn installed_manifest_rejects_non_middleware_stage_drift() {
    let mut manifest = source_manifest();
    manifest.contributes.insert(
        Capability::Management,
        ContributionDeclaration {
            id: "9acme.request-tags.management".into(),
            version: 1,
            stages: vec![Stage::Registration],
            input_formats: Vec::new(),
            output_formats: Vec::new(),
        },
    );

    assert_eq!(manifest.validate(), Err(ManifestError::Invalid));
}

#[test]
fn plugin_identity_rejects_invalid_segments_and_overlong_ids() {
    let mut manifest = source_manifest();
    manifest.publisher = "Acme".into();
    assert_eq!(manifest.plugin_id(), Err(ManifestError::Invalid));

    let mut manifest = source_manifest();
    manifest.name = "request-tags-".into();
    assert_eq!(manifest.plugin_id(), Err(ManifestError::Invalid));

    let mut manifest = source_manifest();
    manifest.publisher = "a".repeat(32);
    manifest.name = "b".repeat(32);
    assert_eq!(manifest.plugin_id(), Err(ManifestError::Invalid));
}

#[test]
fn state_namespaces_are_base_infrastructure_with_bounded_versions() {
    let mut manifest = source_manifest();
    manifest.state = vec![gateway_plugin_sdk::StateNamespace {
        namespace: "cache.primary".into(),
        schema_version: 2,
        schema: json!({"type":"object"}),
        maximum_records: 10,
        maximum_bytes: 4096,
        maximum_value_bytes: 1024,
        migrates_from: vec![1],
    }];
    assert!(manifest.validate().is_ok());
    manifest.state[0].migrates_from.push(2);
    assert_eq!(manifest.validate(), Err(ManifestError::Invalid));
}

#[test]
fn public_manifest_fields_use_only_the_v3_camel_case_shape() {
    let value = serde_json::to_value(source_manifest()).unwrap();
    assert!(value.get("manifestVersion").is_some());
    assert!(value.get("displayName").is_some());
    assert!(value.get("configurationSchema").is_some());
    assert!(value.get("schema_version").is_none());

    let mut old = value;
    old.as_object_mut()
        .unwrap()
        .insert("schema_version".into(), json!(3));
    assert!(serde_json::from_value::<Manifest>(old).is_err());
}

#[test]
fn contributes_rejects_duplicate_capability_keys_in_direct_json_parsing() {
    let declaration = r#"{
        "id":"test.example.middleware","version":4,"stages":["request"],
        "inputFormats":["openai"],"outputFormats":["openai"]
    }"#;
    let manifest = format!(
        r#"{{
            "manifestVersion":2,"name":"example","displayName":"Example","publisher":"test",
            "version":"1.0.0","description":"Example","license":"MIT",
            "engines":{{"codex-proxy-rs":"*"}},"main":"bin/worker","runtime":"trustedProcess",
            "contributes":{{"middleware":{declaration},"middleware":{declaration}}}
        }}"#
    );
    assert!(serde_json::from_str::<Manifest>(&manifest).is_err());
    assert_eq!(
        Manifest::from_author_slice(manifest.as_bytes()),
        Err(ManifestError::Invalid)
    );

    let handshake = format!(
        r#"{{
            "protocol_version":4,"artifact_sha256":"digest","plugin_id":"test.example",
            "instance_id":"instance","generation":1,"incarnation":"incarnation",
            "configuration":{{}},
            "contributes":{{"middleware":{declaration},"middleware":{declaration}}}
        }}"#
    );
    assert!(serde_json::from_str::<Handshake>(&handshake).is_err());

    let registration =
        format!(r#"{{"contributes":{{"middleware":{declaration},"middleware":{declaration}}}}}"#);
    assert!(serde_json::from_str::<Registration>(&registration).is_err());
}

#[test]
fn contribution_declaration_uses_camel_case_description_fields() {
    let declaration = ContributionDeclaration {
        id: "test.example.middleware".into(),
        version: 4,
        stages: vec![Stage::Request],
        input_formats: vec!["openai".into()],
        output_formats: vec!["openai".into()],
    };
    assert_eq!(
        serde_json::to_value(declaration).unwrap(),
        json!({
            "id":"test.example.middleware",
            "version":4,
            "stages":["request"],
            "inputFormats":["openai"],
            "outputFormats":["openai"]
        })
    );
}
