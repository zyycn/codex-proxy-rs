//! 插件打包命令行的参数、归档校验与输出行为测试

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Read as _,
    process::{Command, Output},
};

use flate2::read::MultiGzDecoder;
use gateway_plugin_sdk::{Capability, MANIFEST_VERSION, Manifest, PROTOCOL_VERSION, Stage};
use sha2::{Digest as _, Sha256};

fn cli(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cpr-plugin"))
        .args(arguments)
        .output()
        .unwrap()
}

fn assert_rejected(arguments: &[&str], message: &str) {
    let output = cli(arguments);
    assert!(
        !output.status.success(),
        "{arguments:?} unexpectedly succeeded"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(message),
        "{arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn package_arguments() -> Vec<&'static str> {
    vec![
        "package",
        "--manifest",
        "plugin.json",
        "--binary",
        "plugin-bin",
        "--target",
        "x86_64-unknown-linux-gnu",
        "--output-dir",
        "dist",
    ]
}

#[test]
fn root_help_lists_package_command() {
    let output = cli(&["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("cpr-plugin <COMMAND>"), "{help}");
    assert!(help.contains("package"), "{help}");
}

#[test]
fn version_uses_the_user_command_name() {
    let output = cli(&["--version"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("cpr-plugin {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn package_help_does_not_require_inputs() {
    let output = cli(&["package", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("cpr-plugin package"), "{help}");
    assert!(help.contains("--resource-map"), "{help}");
}

#[test]
fn rejects_missing_subcommand() {
    assert_rejected(&[], "Usage: cpr-plugin");
}

#[test]
fn rejects_unknown_subcommand() {
    assert_rejected(&["publish"], "unrecognized subcommand");
}

#[test]
fn rejects_missing_package_arguments() {
    assert_rejected(&["package"], "--manifest");
}

#[test]
fn rejects_duplicate_single_value_options() {
    let mut arguments = package_arguments();
    arguments.extend(["--manifest", "other.json"]);
    assert_rejected(&arguments, "cannot be used multiple times");
}

#[test]
fn rejects_unknown_targets() {
    let mut arguments = package_arguments();
    arguments[6] = "x86_64-pc-windows-msvc";
    assert_rejected(&arguments, "unsupported plugin target");
}

#[test]
fn rejects_unsafe_resource_mappings_before_reading_files() {
    for (mapping, message) in [
        ("web=../web/dist", "project-relative path"),
        ("web=/web/dist", "project-relative path"),
        ("web=", "project-relative path"),
        ("web/assets=web/dist", "invalid resource-map package prefix"),
        ("web", "must use package-prefix=source-directory"),
    ] {
        let mut arguments = package_arguments();
        arguments.extend(["--resource-map", mapping]);
        assert_rejected(&arguments, message);
    }
}

#[test]
fn rejects_duplicate_resource_prefixes() {
    let mut arguments = package_arguments();
    arguments.extend([
        "--resource-map",
        "web=web/dist",
        "--resource-map",
        "web=other",
    ]);
    assert_rejected(&arguments, "duplicate resource-map package prefix");
}

#[test]
fn packages_only_generated_resources_with_current_metadata() {
    assert_packaged_resources("assets/icon.png", "image/png", b"png");
}

#[test]
fn packages_svg_icons_with_their_declared_mime_and_digest() {
    assert_packaged_resources(
        "assets/icon.svg",
        "image/svg+xml",
        br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M2 2h20v20H2z"/></svg>"#,
    );
}

fn assert_packaged_resources(icon_path: &str, icon_mime: &str, icon: &[u8]) {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("example");
    fs::create_dir_all(project.join("web/dist")).unwrap();
    fs::create_dir_all(project.join("assets")).unwrap();
    let mut source: serde_json::Value = serde_json::from_slice(source_manifest()).unwrap();
    source["icon"] = icon_path.into();
    let resources = source["resources"].as_object_mut().unwrap();
    resources.remove("assets/icon.png");
    resources.insert(icon_path.into(), icon_mime.into());
    fs::write(
        project.join("plugin.json"),
        serde_json::to_vec(&source).unwrap(),
    )
    .unwrap();
    fs::write(project.join("LICENSE"), b"license").unwrap();
    fs::write(project.join(icon_path), icon).unwrap();
    fs::write(project.join("plugin-bin"), b"binary").unwrap();
    fs::write(project.join("web/index.html"), b"source html").unwrap();
    fs::write(project.join("web/dist/index.html"), b"built html").unwrap();
    fs::write(project.join("web/dist/app.js"), b"built js").unwrap();
    fs::write(project.join("web/dist/app.css"), b"built css").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cpr-plugin"))
        .arg("package")
        .arg("--manifest")
        .arg(project.join("plugin.json"))
        .arg("--binary")
        .arg(project.join("plugin-bin"))
        .args(["--target", "x86_64-unknown-linux-gnu"])
        .arg("--output-dir")
        .arg(project.join("dist"))
        .args(["--resource-map", "web=web/dist"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let archive_name = "codex-proxy.request-workbench-0.1.0-x86_64-unknown-linux-gnu.tar.gz";
    let archive_path = project.join("dist").join(archive_name);
    let checksum_path = project.join("dist").join(format!("{archive_name}.sha256"));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{}\n{}\n", archive_path.display(), checksum_path.display())
    );
    let archive = File::open(&archive_path).unwrap();
    let mut package = tar::Archive::new(MultiGzDecoder::new(archive));
    let mut entries = BTreeMap::new();
    for entry in package.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        entries.insert(path, bytes);
    }
    assert_eq!(
        entries.keys().cloned().collect::<Vec<_>>(),
        vec![
            "LICENSE".to_owned(),
            icon_path.to_owned(),
            "bin/plugin".to_owned(),
            "plugin.json".to_owned(),
            "web/app.css".to_owned(),
            "web/app.js".to_owned(),
            "web/index.html".to_owned(),
        ]
    );
    assert_eq!(entries["web/index.html"], b"built html");
    let manifest: Manifest = serde_json::from_slice(&entries["plugin.json"]).unwrap();
    assert_eq!(manifest.manifest_version, MANIFEST_VERSION);
    assert_eq!(
        manifest.contributes[&Capability::Middleware].id,
        "codex-proxy.request-workbench.middleware"
    );
    assert_eq!(
        manifest.contributes[&Capability::Management].stages,
        [Stage::Management]
    );
    assert_eq!(manifest.resources[icon_path], icon_mime);
    assert_eq!(entries[icon_path], icon);
    let package = manifest.package.unwrap();
    assert_eq!(package.files[icon_path], hex::encode(Sha256::digest(icon)));
    assert_eq!(package.protocol_version, PROTOCOL_VERSION);
    assert_eq!(package.target.os, "linux");
    assert_eq!(package.target.architecture, "x86_64");
    assert_eq!(package.files.len(), 6);
    let checksum = fs::read_to_string(checksum_path).unwrap();
    let digest = hex::encode(Sha256::digest(fs::read(archive_path).unwrap()));
    assert_eq!(checksum, format!("{digest}  {archive_name}\n"));
}

fn source_manifest() -> &'static [u8] {
    br#"{
      "manifestVersion":2,
      "name":"request-workbench",
      "displayName":"Request Workbench",
      "publisher":"codex-proxy",
      "version":"0.1.0",
      "description":"test package",
      "license":"Apache-2.0",
      "engines":{"codex-proxy-rs":">=3.12.0, <4.0.0"},
      "main":"bin/plugin",
      "runtime":"trustedProcess",
      "contributes":{
        "middleware":{"version":4,"stages":["request"],"inputFormats":["openai"],"outputFormats":["openai"]},
        "management":{}
      },

      "icon":"assets/icon.png",
      "resources":{"LICENSE":"text/plain","assets/icon.png":"image/png","web/index.html":"text/html","web/app.js":"text/javascript","web/app.css":"text/css"}
    }"#
}
