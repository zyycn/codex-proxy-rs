//! 验证插件归档摘要、资源清单、解压限制与图标绑定

use std::collections::BTreeMap;

use gateway_admin::{
    model::plugins::{PluginArtifactIcon, PluginIconTheme},
    ports::plugins::PluginPackageInspector as _,
};
use gateway_plugin_runtime::{PackageError, PackageInspector, PackageLimits, ValidatedPackage};
use image::{
    DynamicImage, ExtendedColorType, Frame, ImageEncoder as _, ImageFormat, RgbaImage,
    codecs::{gif::GifEncoder, png::PngEncoder},
};
use sha2::{Digest as _, Sha256};

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut encoded = Vec::new();
    PngEncoder::new(&mut encoded)
        .write_image(
            &vec![0; width as usize * height as usize * 4],
            width,
            height,
            ExtendedColorType::Rgba8,
        )
        .unwrap();
    encoded
}

fn raster(format: ImageFormat) -> Vec<u8> {
    let mut output = std::io::Cursor::new(Vec::new());
    let image = if format == ImageFormat::Jpeg {
        DynamicImage::new_rgb8(2, 3)
    } else {
        DynamicImage::new_rgba8(2, 3)
    };
    image.write_to(&mut output, format).unwrap();
    output.into_inner()
}

fn gif(frames: usize) -> Vec<u8> {
    let mut output = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut output);
        for _ in 0..frames {
            encoder
                .encode_frame(Frame::new(RgbaImage::new(2, 3)))
                .unwrap();
        }
    }
    output
}

fn package_with_icon(path: &str, content_type: &str, icon: Vec<u8>) -> std::sync::Arc<[u8]> {
    let worker = b"worker".to_vec();
    let package_files = BTreeMap::from([
        (
            "bin/worker".to_owned(),
            hex::encode(Sha256::digest(&worker)),
        ),
        (path.to_owned(), hex::encode(Sha256::digest(&icon))),
    ]);
    let resources = BTreeMap::from([(path.to_owned(), content_type.to_owned())]);
    let manifest = serde_json::json!({
        "manifestVersion":2,
        "name": "icon",
        "displayName": "Icon",
        "publisher": "test",
        "version": "1.0.0",
        "description": "Icon fixture",
        "license": "MIT",
        "engines": {"codex-proxy-rs": ">=1.0.0, <2.0.0"},
        "main": "bin/worker",
        "runtime": "trustedProcess",
        "contributes": {},
        "resources": resources,
        "icon": path,
        "package": {
            "protocolVersion":2,
            "target": {
                "os": std::env::consts::OS,
                "architecture": std::env::consts::ARCH
            },
            "files": package_files
        }
    });
    crate::support::archive(BTreeMap::from([
        ("plugin.json".into(), serde_json::to_vec(&manifest).unwrap()),
        ("bin/worker".into(), worker),
        (path.to_owned(), icon),
    ]))
}

#[test]
fn expected_digest_is_checked_before_archive_parsing() {
    assert!(matches!(
        ValidatedPackage::read(
            b"invalid".as_slice().into(),
            Some(&"0".repeat(64)),
            PackageLimits::default()
        ),
        Err(PackageError::Digest)
    ));
}

#[test]
fn unlisted_content_is_rejected() {
    let bytes = crate::support::archive(BTreeMap::from([("surprise".into(), b"code".to_vec())]));
    assert!(matches!(
        ValidatedPackage::read(bytes, None, PackageLimits::default()),
        Err(PackageError::Archive)
    ));
}

#[test]
fn decompression_limit_includes_archive_overhead() {
    let bytes = crate::support::package(&[0; 4096]);
    assert!(matches!(
        ValidatedPackage::read(
            bytes,
            None,
            PackageLimits {
                expanded_bytes: 1024,
                ..PackageLimits::default()
            }
        ),
        Err(PackageError::Limit)
    ));
}

#[test]
fn package_retains_the_exact_verified_archive_for_recovery() {
    let bytes = crate::support::package(b"hello");
    let package = ValidatedPackage::read(bytes.clone(), None, PackageLimits::default()).unwrap();
    assert_eq!(package.archive(), bytes);
    assert_eq!(package.manifest().plugin_id().unwrap(), "test.example");
}

#[tokio::test]
async fn validated_icon_is_exposed_from_the_digest_bound_archive() {
    let bytes = package_with_icon("assets/icon.png", "image/png", png(2, 3));
    let digest = hex::encode(Sha256::digest(&bytes));
    let inspector = PackageInspector::new(PackageLimits::default(), "1.0.0".parse().unwrap());
    let artifact = inspector
        .inspect(bytes, Some(digest.clone()))
        .await
        .unwrap();
    assert_eq!(
        artifact.metadata.icon,
        Some(PluginArtifactIcon::Path("assets/icon.png".into()))
    );
    let icon = inspector
        .icon(artifact.archive, digest, PluginIconTheme::Dark)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(icon.content_type, "image/png");
    assert!(!icon.body.is_empty());
}

#[tokio::test]
async fn common_icon_formats_are_verified_and_returned_without_transcoding() {
    for (path, mime, bytes) in [
        ("assets/icon.PNG", "image/png", png(2, 3)),
        ("assets/icon.jpeg", "image/jpeg", raster(ImageFormat::Jpeg)),
        ("assets/icon.jfif", "image/jpeg", raster(ImageFormat::Jpeg)),
        ("assets/icon.WebP", "image/webp", raster(ImageFormat::WebP)),
        ("assets/icon.gif", "image/gif", gif(2)),
        ("assets/icon.ico", "image/x-icon", raster(ImageFormat::Ico)),
        (
            "assets/icon.ICO",
            "image/vnd.microsoft.icon",
            raster(ImageFormat::Ico),
        ),
        ("assets/icon.bmp", "image/bmp", raster(ImageFormat::Bmp)),
        (
            "assets/icon.BMP",
            "image/x-ms-bmp",
            raster(ImageFormat::Bmp),
        ),
        (
            "assets/icon.SVG",
            "image/svg+xml",
            br##"<?xml version="1.0" encoding="UTF-8"?>
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
              <style>.icon { fill: url(#paint); }</style>
              <defs><linearGradient id="paint"><stop stop-color="#58f"/>
                <stop offset="1" stop-color="#8ef"/></linearGradient></defs>
              <path class="icon" d="M2 2h20v20H2z"/>
            </svg>"##
                .to_vec(),
        ),
    ] {
        let archive = package_with_icon(path, mime, bytes.clone());
        let digest = hex::encode(Sha256::digest(&archive));
        let inspector = PackageInspector::new(PackageLimits::default(), "1.0.0".parse().unwrap());
        let artifact = inspector
            .inspect(archive, Some(digest.clone()))
            .await
            .unwrap_or_else(|error| panic!("{path}: {error:?}"));
        for theme in [PluginIconTheme::Light, PluginIconTheme::Dark] {
            let icon = inspector
                .icon(artifact.archive.clone(), digest.clone(), theme)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(icon.content_type, mime, "{path}");
            assert_eq!(icon.body, bytes, "{path}");
        }
    }
}

#[test]
fn svg_icons_reject_malformed_xml_non_svg_roots_and_external_entities() {
    for bytes in [
        b"not an image".as_slice(),
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><path></svg>",
        b"<html xmlns=\"http://www.w3.org/1999/xhtml\"/>",
        b"<svg xmlns=\"https://invalid.example/svg\"/>",
        br#"<!DOCTYPE svg [<!ENTITY external SYSTEM "file:///etc/passwd">]>
            <svg xmlns="http://www.w3.org/2000/svg">&external;</svg>"#,
        br#"<?xml-stylesheet href="https://invalid.example/transform.xsl"?>
            <svg xmlns="http://www.w3.org/2000/svg"/>"#,
    ] {
        let archive = package_with_icon("assets/icon.svg", "image/svg+xml", bytes.to_vec());
        assert_eq!(
            ValidatedPackage::read(archive, None, PackageLimits::default()).err(),
            Some(PackageError::Archive),
            "{}",
            String::from_utf8_lossy(bytes)
        );
    }
}

#[test]
fn icon_validation_keeps_byte_node_and_animation_budgets() {
    let large_svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\"><!--{}--></svg>",
        "x".repeat(512 * 1024)
    );
    let many_nodes = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\">{}</svg>",
        "<path/>".repeat(16_384)
    );
    for (path, mime, bytes) in [
        ("assets/icon.svg", "image/svg+xml", large_svg.into_bytes()),
        ("assets/icon.svg", "image/svg+xml", many_nodes.into_bytes()),
        ("assets/icon.gif", "image/gif", gif(257)),
    ] {
        let archive = package_with_icon(path, mime, bytes);
        assert_eq!(
            ValidatedPackage::read(archive, None, PackageLimits::default()).err(),
            Some(PackageError::Limit),
            "{path}"
        );
    }
}

#[test]
fn icon_content_must_fully_decode_match_its_type_and_fit_dimensions() {
    let valid_png = png(2, 3);
    let wrong_type = package_with_icon("assets/icon.jpg", "image/jpeg", valid_png.clone());
    assert_eq!(
        ValidatedPackage::read(wrong_type, None, PackageLimits::default()).err(),
        Some(PackageError::Archive)
    );

    let truncated = package_with_icon("assets/icon.png", "image/png", valid_png[..32].to_vec());
    assert_eq!(
        ValidatedPackage::read(truncated, None, PackageLimits::default()).err(),
        Some(PackageError::Archive)
    );

    let oversized = package_with_icon("assets/icon.png", "image/png", png(4097, 1));
    assert_eq!(
        ValidatedPackage::read(oversized, None, PackageLimits::default()).err(),
        Some(PackageError::Limit)
    );
}

#[test]
fn obsolete_manifest_fields_are_rejected_without_aliases() {
    let manifest = serde_json::json!({
        "manifestVersion":2,
        "name": "example",
        "displayName": "Example",
        "publisher": "test",
        "version": "1.0.0",
        "description": "Example",
        "license": "MIT",
        "engines": {"codex-proxy-rs": ">=1.0.0, <2.0.0"},
        "main": "bin/worker",
        "runtime": "trustedProcess",
        "capabilities": [{"capability":"request_normalizer"}]
    });
    let archive = crate::support::archive(BTreeMap::from([(
        "plugin.json".to_owned(),
        serde_json::to_vec(&manifest).unwrap(),
    )]));
    assert_eq!(
        ValidatedPackage::read(archive, None, PackageLimits::default()).err(),
        Some(PackageError::Archive)
    );
}

#[test]
fn source_manifest_cannot_be_installed_without_package_metadata() {
    let manifest = serde_json::json!({
        "manifestVersion":2,
        "name": "example",
        "displayName": "Example",
        "publisher": "test",
        "version": "1.0.0",
        "description": "Example",
        "license": "MIT",
        "engines": {"codex-proxy-rs": "*"},
        "main": "bin/worker",
        "runtime": "trustedProcess",
        "contributes": {}
    });
    let archive = crate::support::archive(BTreeMap::from([(
        "plugin.json".to_owned(),
        serde_json::to_vec(&manifest).unwrap(),
    )]));
    assert_eq!(
        ValidatedPackage::read(archive, None, PackageLimits::default()).err(),
        Some(PackageError::Manifest(
            gateway_plugin_sdk::ManifestError::Invalid
        ))
    );
}

#[test]
fn duplicate_contribution_keys_are_rejected_before_map_overwrite() {
    let manifest = br#"{
        "manifestVersion":2,
        "name":"example",
        "displayName":"Example",
        "publisher":"test",
        "version":"1.0.0",
        "description":"Example",
        "license":"MIT",
        "engines":{"codex-proxy-rs":"*"},
        "main":"bin/worker",
        "runtime":"trustedProcess",
        "contributes":{
            "middleware":{"id":"test.example.first","version":4,"stages":["request"]},
            "middleware":{"id":"test.example.second","version":4,"stages":["request"]}
        }
    }"#;
    let archive = crate::support::archive(BTreeMap::from([(
        "plugin.json".to_owned(),
        manifest.to_vec(),
    )]));
    assert_eq!(
        ValidatedPackage::read(archive, None, PackageLimits::default()).err(),
        Some(PackageError::Archive)
    );
}
