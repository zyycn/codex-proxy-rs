use std::path::PathBuf;

use gateway_host::config::{FileLoggingConfig, HostConfig, ListenConfig, LoggingConfig};
use gateway_host::system_update::SystemUpdateConfig;

#[test]
fn logging_requires_at_least_one_sink() {
    let mut config = HostConfig {
        listen: ListenConfig {
            host: "127.0.0.1".to_owned(),
            port: 8080,
        },
        logging: LoggingConfig {
            level: "info".to_owned(),
            stdout: false,
            file: FileLoggingConfig {
                enabled: false,
                directory: PathBuf::from("logs"),
                retention_days: 7,
                max_file_size_mb: 100,
                max_files: 30,
            },
        },
        system_update: SystemUpdateConfig::default(),
        drain_timeout_seconds: 30,
        worker_shutdown_timeout_seconds: 30,
    };

    assert!(
        config
            .resolve_and_validate(std::path::Path::new("/tmp"))
            .is_err()
    );
}
