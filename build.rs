use std::{env, fs};

fn main() {
    println!("cargo:rerun-if-changed=VERSION");
    println!("cargo:rerun-if-env-changed=CPR_VERSION");
    println!("cargo:rerun-if-env-changed=CPR_GIT_SHA");
    println!("cargo:rerun-if-env-changed=CPR_BUILD_TIME");
    println!("cargo:rerun-if-env-changed=CPR_BUILD_TYPE");

    let version = version_env_value("CPR_VERSION")
        .or_else(version_file)
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

fn version_file() -> Option<String> {
    fs::read_to_string("VERSION")
        .ok()
        .map(|value| value.trim().trim_start_matches('v').to_string())
        .filter(|value| !value.is_empty())
}
