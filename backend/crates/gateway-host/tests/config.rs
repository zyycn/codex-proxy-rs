use std::path::PathBuf;

use gateway_host::config::{FileLoggingConfig, HostConfig, ListenConfig, LoggingConfig};
use gateway_host::system_update::SystemUpdateConfig;

#[test]
fn host_config_resolves_only_host_owned_relative_paths() {
    let mut config = valid_config();
    config
        .resolve_and_validate(std::path::Path::new("/srv/gateway"))
        .expect("valid host config");

    assert_eq!(
        config.logging.file.directory,
        PathBuf::from("/srv/gateway/.runtime/logs")
    );
}

#[test]
fn host_config_rejects_zero_drain_window() {
    let mut config = valid_config();
    config.drain_timeout_seconds = 0;

    assert!(
        config
            .resolve_and_validate(std::path::Path::new("/srv/gateway"))
            .is_err()
    );
}

fn valid_config() -> HostConfig {
    let system_update = SystemUpdateConfig {
        update_state_file: PathBuf::from(".runtime/update-state.json"),
        update_lock_file: PathBuf::from(".runtime/update-state.lock"),
        update_temp_dir: PathBuf::from(".runtime/update-tmp"),
        ..SystemUpdateConfig::default()
    };
    HostConfig {
        listen: ListenConfig {
            host: "127.0.0.1".to_owned(),
            port: 8080,
        },
        logging: LoggingConfig {
            level: "info".to_owned(),
            stdout: true,
            file: FileLoggingConfig {
                enabled: true,
                directory: PathBuf::from(".runtime/logs"),
                retention_days: 7,
                max_file_size_mb: 100,
                max_files: 30,
            },
        },
        system_update,
        drain_timeout_seconds: 30,
        worker_shutdown_timeout_seconds: 30,
    }
}
