use std::path::PathBuf;

use gateway_host::config::{FileLoggingConfig, HostConfig, ListenConfig, LoggingConfig};
use gateway_host::system_update::SystemUpdateConfig;

#[test]
fn system_update_defaults_should_use_host_build_metadata_and_official_repository() {
    let config = SystemUpdateConfig::default();

    assert_eq!(
        (
            config.version.as_str(),
            config.build_type.as_str(),
            config.update_repository.as_deref(),
        ),
        (
            env!("CPR_VERSION"),
            env!("CPR_BUILD_TYPE"),
            Some("zyycn/codex-proxy-rs"),
        )
    );
}

#[test]
fn host_config_should_default_update_assets_to_the_resolved_api_directory() {
    let mut config = valid_config();
    config.system_update.web_dist_dir = None;
    let assets = std::path::Path::new("/srv/gateway/web/dist");

    config
        .resolve_and_validate(std::path::Path::new("/srv/gateway/deploy"), assets)
        .expect("host config");

    assert_eq!(config.system_update.web_dist_dir.as_deref(), Some(assets));
}

#[test]
fn host_config_should_preserve_explicit_update_asset_directory() {
    let mut config = valid_config();
    config.system_update.web_dist_dir = Some(PathBuf::from("../custom/dist"));

    config
        .resolve_and_validate(
            std::path::Path::new("/srv/gateway/deploy"),
            std::path::Path::new("/srv/gateway/web/dist"),
        )
        .expect("host config");

    assert_eq!(
        config.system_update.web_dist_dir,
        Some(PathBuf::from("/srv/gateway/deploy/../custom/dist"))
    );
}

#[test]
fn host_config_should_reject_empty_update_asset_directory() {
    let mut config = valid_config();
    config.system_update.web_dist_dir = Some(PathBuf::new());

    assert!(matches!(
        config.resolve_and_validate(
            std::path::Path::new("/srv/gateway/deploy"),
            std::path::Path::new("/srv/gateway/web/dist"),
        ),
        Err(gateway_host::ConfigError::InvalidField(
            "host.system_update.web_dist_dir"
        ))
    ));
}

#[test]
fn host_config_resolves_only_host_owned_relative_paths() {
    let mut config = valid_config();
    config
        .resolve_and_validate(
            std::path::Path::new("/srv/gateway"),
            std::path::Path::new("/srv/gateway/web/dist"),
        )
        .expect("valid host config");

    assert_eq!(
        config.logging.file.directory,
        PathBuf::from("/srv/gateway/.runtime/logs")
    );
}

#[test]
fn host_config_resolves_runtime_data_dir_relative_to_configuration() {
    let mut config = valid_config();
    config
        .resolve_and_validate(
            std::path::Path::new("/srv/gateway"),
            std::path::Path::new("/srv/gateway/web/dist"),
        )
        .expect("valid host config");

    assert_eq!(
        config.runtime_data_dir(),
        PathBuf::from("/srv/gateway/runtime-data")
    );
}

#[test]
fn host_config_derives_update_paths_from_runtime_data_dir() {
    let mut config = valid_config();
    config
        .resolve_and_validate(
            std::path::Path::new("/srv/gateway"),
            std::path::Path::new("/srv/gateway/web/dist"),
        )
        .expect("valid host config");

    assert_eq!(
        (
            config.system_update.update_state_file,
            config.system_update.update_lock_file,
            config.system_update.update_temp_dir,
        ),
        (
            PathBuf::from("/srv/gateway/runtime-data/update-state.json"),
            PathBuf::from("/srv/gateway/runtime-data/update.lock"),
            PathBuf::from("/srv/gateway/runtime-data/update-tmp"),
        )
    );
}

#[test]
fn host_config_rejects_zero_drain_window() {
    let mut config = valid_config();
    config.drain_timeout_seconds = 0;

    assert!(
        config
            .resolve_and_validate(
                std::path::Path::new("/srv/gateway"),
                std::path::Path::new("/srv/gateway/web/dist"),
            )
            .is_err()
    );
}

#[test]
fn logging_rejects_zero_retention_windows() {
    for request_dump in [false, true] {
        let mut config = valid_config();
        if request_dump {
            config.logging.request_dump_retention_days = 0;
        } else {
            config.logging.file.retention_days = 0;
        }
        assert!(
            config
                .resolve_and_validate(
                    std::path::Path::new("/srv/gateway"),
                    std::path::Path::new("/srv/gateway/web/dist"),
                )
                .is_err()
        );
    }
}

#[test]
fn logging_defaults_to_seven_days_and_one_day_for_payloads_and_rejects_count_retention() {
    let mut value = serde_json::json!({
        "level": "info", "stdout": true,
        "file": { "enabled": true, "directory": "logs", "max_file_size_mb": 20 }
    });
    let config: LoggingConfig = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(config.request_dump_retention_days, 1);
    assert_eq!(config.file.retention_days, 7);
    assert!(!config.oauth_recovery);
    value["file"]["max_files"] = serde_json::json!(20);
    assert!(serde_json::from_value::<LoggingConfig>(value).is_err());
}

#[test]
fn dedicated_file_channels_can_be_enabled_without_application_sinks() {
    for oauth_recovery in [false, true] {
        let mut config = valid_config();
        config.logging.stdout = false;
        config.logging.file.enabled = false;
        config.logging.oauth_recovery = oauth_recovery;
        config.logging.request_dump = !oauth_recovery;
        config
            .resolve_and_validate(
                std::path::Path::new("/srv/gateway"),
                std::path::Path::new("/srv/gateway/web/dist"),
            )
            .unwrap();
    }
}

fn valid_config() -> HostConfig {
    let system_update = SystemUpdateConfig {
        update_state_file: PathBuf::from("update-state.json"),
        update_lock_file: PathBuf::from("update.lock"),
        update_temp_dir: PathBuf::from("update-tmp"),
        ..SystemUpdateConfig::default()
    };
    HostConfig {
        listen: ListenConfig {
            host: "127.0.0.1".to_owned(),
            port: 8080,
        },
        runtime_data_dir: PathBuf::from("runtime-data"),
        logging: LoggingConfig {
            level: "info".to_owned(),
            stdout: true,
            file: FileLoggingConfig {
                enabled: true,
                directory: PathBuf::from(".runtime/logs"),
                retention_days: 7,
                max_file_size_mb: 100,
            },
            oauth_recovery: false,
            request_dump: false,
            request_dump_retention_days: 1,
        },
        system_update,
        drain_timeout_seconds: 30,
        worker_shutdown_timeout_seconds: 30,
    }
}
