use std::{env, fs};

const RELEASE_VERSION_FILE: &str = "../../../release/version.yaml";

fn main() {
    println!("cargo:rerun-if-changed={RELEASE_VERSION_FILE}");
    println!("cargo:rerun-if-env-changed=CPR_VERSION");
    println!("cargo:rerun-if-env-changed=CPR_GIT_SHA");
    println!("cargo:rerun-if-env-changed=CPR_BUILD_TIME");
    println!("cargo:rerun-if-env-changed=CPR_BUILD_TYPE");

    let version = version_env_value("CPR_VERSION")
        .or_else(release_version)
        .unwrap_or_else(|| {
            env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0-dev".to_string())
        });
    let git_sha = env_value("CPR_GIT_SHA").unwrap_or_else(|| "unknown".to_string());
    let build_time = env_value("CPR_BUILD_TIME").unwrap_or_else(|| "unknown".to_string());
    let build_type = env_value("CPR_BUILD_TYPE").unwrap_or_else(|| "source".to_string());

    println!("cargo:rustc-env=CPR_VERSION={version}");
    println!("cargo:rustc-env=CPR_GIT_SHA={git_sha}");
    println!("cargo:rustc-env=CPR_BUILD_TIME={build_time}");
    println!("cargo:rustc-env=CPR_BUILD_TYPE={build_type}");
}

fn env_value(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn version_env_value(key: &str) -> Option<String> {
    env_value(key).map(|value| value.trim_start_matches('v').to_string())
}

fn release_version() -> Option<String> {
    let data = fs::read_to_string(RELEASE_VERSION_FILE).ok()?;
    yaml_string_value(&data, "version").map(|value| value.trim_start_matches('v').to_string())
}

fn yaml_string_value(data: &str, key: &str) -> Option<String> {
    for line in data.lines() {
        let line = line.split_once('#').map_or(line, |(value, _)| value).trim();
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim() != key {
            continue;
        }
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let first = value.chars().next()?;
        let parsed = if first == '"' || first == '\'' {
            let rest = &value[first.len_utf8()..];
            let end = rest.find(first)?;
            rest[..end].trim()
        } else {
            value.trim()
        };
        if !parsed.is_empty() {
            return Some(parsed.to_string());
        }
    }
    None
}
