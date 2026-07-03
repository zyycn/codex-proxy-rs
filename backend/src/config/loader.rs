use std::{env, path::Path};

use super::types::AppConfig;

const CONFIG_FILE_ENV: &str = "CPR_CONFIG_FILE";

impl AppConfig {
    /// 从 `CPR_CONFIG_FILE` 或当前目录 `config.yaml` 加载配置文件。
    pub fn load() -> Result<Self, ::config::ConfigError> {
        let config_file = env::var(CONFIG_FILE_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty());
        match config_file {
            Some(path) => load_file(path),
            None => Self::load_from_dir("."),
        }
    }

    /// 从指定目录加载配置文件。
    pub fn load_from_dir(config_dir: impl AsRef<Path>) -> Result<Self, ::config::ConfigError> {
        let config_dir = config_dir.as_ref();
        load_file(config_dir.join("config.yaml"))
    }
}

fn load_file(config_file: impl AsRef<Path>) -> Result<AppConfig, ::config::ConfigError> {
    ::config::Config::builder()
        .add_source(::config::File::from(config_file.as_ref()).required(true))
        .build()?
        .try_deserialize()
}
